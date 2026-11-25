use crate as pallet_asset_weight;
use crate::{ContractAssetInspector, OraclePriceProvider, OracleReadError};
use frame_support::{derive_impl, parameter_types};
use frame_system::EnsureRoot;
use pallet_oracle_registry::OracleId;
use sp_runtime::BuildStorage;
use std::cell::RefCell;
use std::collections::BTreeMap;

type Block = frame_system::mocking::MockBlock<Test>;

frame_support::construct_runtime!(
    pub enum Test {
        System: frame_system,
        OracleRegistry: pallet_oracle_registry,
        OracleLedger: pallet_oracle_ledger,
        AssetWeight: pallet_asset_weight,
    }
);

#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
    type Block = Block;
}

parameter_types! {
    pub const MaxSigners: u32 = 8;
    pub const MaxOracleNameLen: u32 = 32;
    pub const MaxOracleMetadataLen: u32 = 64;
}

impl pallet_oracle_registry::Config for Test {
    type RuntimeEvent = RuntimeEvent;
    type AdminOrigin = EnsureRoot<u64>;
    type MaxSigners = MaxSigners;
    type MaxOracleNameLen = MaxOracleNameLen;
    type MaxMetadataLen = MaxOracleMetadataLen;
}

parameter_types! {
    pub const MaxPayloadLen: u32 = 128;
    pub const MaxDomainKeyLen: u32 = 32;
    pub const MaxTxRefLen: u32 = 32;
    pub const MaxExternalTxRefs: u32 = 4;
    pub const MaxLedgerSignatures: u32 = 8;
}

impl pallet_oracle_ledger::Config for Test {
    type RuntimeEvent = RuntimeEvent;
    type MaxPayloadLen = MaxPayloadLen;
    type MaxDomainKeyLen = MaxDomainKeyLen;
    type MaxTxRefLen = MaxTxRefLen;
    type MaxExternalTxRefs = MaxExternalTxRefs;
    type MaxSignatures = MaxLedgerSignatures;
}

thread_local! {
    pub static CONTRACT_EXISTS: RefCell<BTreeMap<u64, bool>> = RefCell::new(BTreeMap::new());
    pub static PRICES: RefCell<BTreeMap<(OracleId, Vec<u8>), Result<u128, OracleReadError>>> = RefCell::new(BTreeMap::new());
    pub static ORACLE_ACTIVE: RefCell<BTreeMap<OracleId, bool>> = RefCell::new(BTreeMap::new());
}

pub struct MockInspector;
impl ContractAssetInspector<u64> for MockInspector {
    fn contract_exists(contract: &u64) -> bool {
        CONTRACT_EXISTS.with(|c| *c.borrow().get(contract).unwrap_or(&false))
    }
    fn total_supply(_contract: &u64) -> Option<u128> {
        None
    }
}

pub struct MockOraclePrices;
impl OraclePriceProvider<MaxDomainKeyLen> for MockOraclePrices {
    fn is_active_oracle(oracle_id: OracleId) -> bool {
        ORACLE_ACTIVE.with(|m| *m.borrow().get(&oracle_id).unwrap_or(&false))
    }
    fn latest_price(oracle_id: OracleId, domain_key: &[u8]) -> Result<u128, OracleReadError> {
        PRICES.with(|p| {
            p.borrow()
                .get(&(oracle_id, domain_key.to_vec()))
                .cloned()
                .unwrap_or(Err(OracleReadError::NoPrice))
        })
    }
}

parameter_types! {
    pub const MaxAssets: u32 = 32;
    pub const BandSize: u128 = 10;
    pub const DissimilarityScale: u128 = 100;
    pub const CrossInfluencePpb: u128 = 500_000_000; // 50% of scaled delta
    pub const SelfInfluencePpb: u128 = 500_000_000;
    pub const MaxBunchAbsorptionPerUpdate: u128 = 2;
    pub const MaxTotalCrossPriceDeltaPerUpdate: u128 = 10;
    pub const MaxCrossPriceDeltaPerUpdate: u128 = 500;
    pub const MaxSelfPriceDeltaPerUpdate: u128 = 500;
    pub const MinEffectivePrice: u128 = 1;
    pub const MaxEffectivePrice: u128 = 1_000_000;
    pub const MaxWeightMagnitude: u128 = 1_000_000;
    pub const MinUnitCap: i128 = 1;
    pub const MaxUnitCap: i128 = 1_000_000;
    pub const DefaultUnitCap: i128 = 1_000;
    pub const MaxUnitSlopePpm: i128 = 1_000_000;
    pub const UnitDistWeightPpb: u128 = 1_000_000_000; // 100% for test visibility; default dist aggregate is 0
    pub const UnitCapSelfPpb: u128 = 100_000_000;
    pub const UnitCapInboundPpb: u128 = 100_000_000;
    pub const UnitCapOutboundPpb: u128 = 100_000_000;
    pub const MaxDistributionAggregate: i128 = 1_000_000_000;
}

impl pallet_asset_weight::Config for Test {
    type RuntimeEvent = RuntimeEvent;
    type AdminOrigin = EnsureRoot<u64>;
    type ContractInspector = MockInspector;
    type OraclePrices = MockOraclePrices;
    type MaxAssets = MaxAssets;
    type BandSize = BandSize;
    type DissimilarityScale = DissimilarityScale;
    type CrossInfluencePpb = CrossInfluencePpb;
    type SelfInfluencePpb = SelfInfluencePpb;
    type MaxBunchAbsorptionPerUpdate = MaxBunchAbsorptionPerUpdate;
    type MaxTotalCrossPriceDeltaPerUpdate = MaxTotalCrossPriceDeltaPerUpdate;
    type MaxCrossPriceDeltaPerUpdate = MaxCrossPriceDeltaPerUpdate;
    type MaxSelfPriceDeltaPerUpdate = MaxSelfPriceDeltaPerUpdate;
    type MinEffectivePrice = MinEffectivePrice;
    type MaxEffectivePrice = MaxEffectivePrice;
    type MaxWeightMagnitude = MaxWeightMagnitude;
    type MinUnitCap = MinUnitCap;
    type MaxUnitCap = MaxUnitCap;
    type DefaultUnitCap = DefaultUnitCap;
    type MaxUnitSlopePpm = MaxUnitSlopePpm;
    type UnitDistWeightPpb = UnitDistWeightPpb;
    type UnitCapSelfPpb = UnitCapSelfPpb;
    type UnitCapInboundPpb = UnitCapInboundPpb;
    type UnitCapOutboundPpb = UnitCapOutboundPpb;
    type MaxDistributionAggregate = MaxDistributionAggregate;
}

pub fn new_test_ext() -> sp_io::TestExternalities {
    frame_system::GenesisConfig::<Test>::default()
        .build_storage()
        .unwrap()
        .into()
}

pub fn set_contract(contract: u64, exists: bool) {
    CONTRACT_EXISTS.with(|c| {
        c.borrow_mut().insert(contract, exists);
    });
}

pub fn set_oracle(oracle_id: OracleId, active: bool) {
    ORACLE_ACTIVE.with(|m| {
        m.borrow_mut().insert(oracle_id, active);
    });
}

pub fn set_price(oracle_id: OracleId, domain: &[u8], price: Result<u128, OracleReadError>) {
    PRICES.with(|p| {
        p.borrow_mut().insert((oracle_id, domain.to_vec()), price);
    });
}
