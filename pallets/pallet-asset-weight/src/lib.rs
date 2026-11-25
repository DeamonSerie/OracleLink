#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub use pallet::*;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

use alloc::{collections::BTreeMap, vec::Vec};
use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use frame_support::pallet_prelude::*;
use frame_system::pallet_prelude::*;
use pallet_oracle_ledger as ledger;
use pallet_oracle_registry::{self as registry, OracleId, OracleStatus};
use sp_runtime::RuntimeDebug;

#[derive(
    Clone,
    Copy,
    Encode,
    Decode,
    DecodeWithMemTracking,
    PartialEq,
    Eq,
    RuntimeDebug,
    TypeInfo,
    MaxEncodedLen,
    Default,
)]
pub enum AssetKind {
    #[default]
    Fungible,
    NonFungible,
}

/// Narrow contract/asset existence and supply inspection.
///
/// Runtime can wire this to `pallet-contracts` queries where version-compatible.
/// The default/manual implementation used in tests can report existence/supply from
/// explicit registration parameters.
pub trait ContractAssetInspector<AccountId> {
    fn contract_exists(_contract: &AccountId) -> bool {
        false
    }
    fn total_supply(_contract: &AccountId) -> Option<u128> {
        None
    }
}

impl<AccountId> ContractAssetInspector<AccountId> for () {}

/// Narrow oracle-read adapter. Implementations read registry and ledger storage.
pub trait OraclePriceProvider<MaxDomainKeyLen: Get<u32>> {
    fn is_active_oracle(oracle_id: OracleId) -> bool;
    fn latest_price(oracle_id: OracleId, domain_key: &[u8]) -> Result<u128, OracleReadError>;
}

#[derive(
    Clone, Copy, Encode, Decode, DecodeWithMemTracking, PartialEq, Eq, RuntimeDebug, TypeInfo,
)]
pub enum OracleReadError {
    OracleNotFound,
    OracleInactive,
    NoPrice,
    DomainKeyTooLarge,
    InvalidPayload,
    Conflicted,
}

#[derive(Encode, Decode, DecodeWithMemTracking, RuntimeDebug, TypeInfo, MaxEncodedLen)]
#[scale_info(skip_type_params(T))]
pub struct AssetConfig<T: Config> {
    pub contract: T::AccountId,
    pub kind: AssetKind,
    pub oracle_id: OracleId,
    pub domain_key: BoundedVec<u8, T::MaxDomainKeyLen>,
    pub decimals: u8,
    pub registered_at: BlockNumberFor<T>,
    pub owner: T::AccountId,
}

impl<T: Config> Clone for AssetConfig<T> {
    fn clone(&self) -> Self {
        Self {
            contract: self.contract.clone(),
            kind: self.kind,
            oracle_id: self.oracle_id,
            domain_key: self.domain_key.clone(),
            decimals: self.decimals,
            registered_at: self.registered_at,
            owner: self.owner.clone(),
        }
    }
}
impl<T: Config> PartialEq for AssetConfig<T> {
    fn eq(&self, other: &Self) -> bool {
        self.contract == other.contract
            && self.kind == other.kind
            && self.oracle_id == other.oracle_id
            && self.domain_key == other.domain_key
            && self.decimals == other.decimals
            && self.registered_at == other.registered_at
            && self.owner == other.owner
    }
}
impl<T: Config> Eq for AssetConfig<T> {}

#[derive(
    Clone,
    Copy,
    Encode,
    Decode,
    DecodeWithMemTracking,
    PartialEq,
    Eq,
    RuntimeDebug,
    TypeInfo,
    MaxEncodedLen,
    Default,
)]
pub struct AssetWeightState<BlockNumber> {
    /// Oracle-sourced price used as propagation input. Only oracle reads update it.
    pub last_price: u128,
    /// Pallet-computed adjusted price. Never written to oracle ledger.
    pub effective_price: u128,
    pub base_weight: i128,
    pub effective_weight: i128,
    pub total_supply: u128,
    pub transfer_count: u64,
    pub last_update: BlockNumber,
    /// Affine base weight per virtual fungible unit.
    pub unit_base_w: i128,
    /// Signed ppm slope per rank; v1 constrained >= 0 for O(1) aggregate clamp boundary.
    pub unit_slope_ppm: i128,
    /// Dynamic per-unit absolute cap; never below `MinUnitCap` or above `MaxUnitCap`.
    pub unit_cap_w: i128,
    /// Cached deterministic aggregate distribution contribution.
    pub dist_aggregate: i128,
    /// Last absorbed inbound cross effective-weight signal applied to this asset.
    pub last_inbound_cross_w: i128,
    /// Last outbound cross effective-weight signal emitted by this asset as source.
    pub last_outbound_cross_w: i128,
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;

    /// Storage layout changed: AssetWeightState gained `effective_price`, AssetConfig lost
    /// `bunch_id`, and bunch storage was removed. Dev/runtime resets are expected; no on-chain
    /// migration is provided for pre-existing state.
    ///
    /// v2 semantic/layout note: propagation config renamed/redefined (`SimilarityScale` ->
    /// `DissimilarityScale`) and added bunch overflow config/constants; `PricePropagated` event
    /// extended with budget/absorption summary fields. No persistent storage layout migration is
    /// provided; dev/runtime resets are expected.
    ///
    /// v3 semantic/layout note: `AssetWeightState` gained deterministic per-unit fungible
    /// distribution fields (`unit_base_w`, `unit_slope_ppm`, `unit_cap_w`, `dist_aggregate`,
    /// cap-update signal accumulators). Distribution is a separate, O(1) layer: `effective_weight`
    /// becomes `base_weight + dist_component` (clamped), and propagation uses effective-weight
    /// deltas. No on-chain migration provided; dev/runtime resets are expected.
    const STORAGE_VERSION: StorageVersion = StorageVersion::new(3);

    #[pallet::config]
    pub trait Config: frame_system::Config + registry::Config + ledger::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

        /// Admin/origin allowed for privileged asset management.
        type AdminOrigin: EnsureOrigin<Self::RuntimeOrigin>;

        /// Contract existence/supply inspection.
        type ContractInspector: ContractAssetInspector<Self::AccountId>;

        /// Oracle price reads over registry+ledger.
        type OraclePrices: OraclePriceProvider<Self::MaxDomainKeyLen>;

        /// Max assets registered in the pallet. Bounds propagation scan.
        #[pallet::constant]
        type MaxAssets: Get<u32>;

        /// Weight-band quantum used for deterministic bunch grouping.
        #[pallet::constant]
        type BandSize: Get<u128>;

        /// Dissimilarity normalization cap. `d(a)=min(distance,DissimilarityScale)`.
        /// Identical weights get `d=0`; distances at/above scale get max dissimilarity.
        #[pallet::constant]
        type DissimilarityScale: Get<u128>;

        /// Cross influence factor, parts-per-billion of source weight delta scaled to price units.
        #[pallet::constant]
        type CrossInfluencePpb: Get<u128>;

        /// Self influence factor, parts-per-billion of source weight delta scaled to price units.
        #[pallet::constant]
        type SelfInfluencePpb: Get<u128>;

        /// Max absolute per-bunch absorption per refresh. Overflow spills to other bunches.
        #[pallet::constant]
        type MaxBunchAbsorptionPerUpdate: Get<u128>;

        /// Max absolute total cross-ripple budget per refresh.
        #[pallet::constant]
        type MaxTotalCrossPriceDeltaPerUpdate: Get<u128>;

        /// Max absolute per-target effective price delta per refresh.
        #[pallet::constant]
        type MaxCrossPriceDeltaPerUpdate: Get<u128>;

        /// Max absolute source effective price delta per refresh.
        #[pallet::constant]
        type MaxSelfPriceDeltaPerUpdate: Get<u128>;

        /// Minimum effective price floor.
        #[pallet::constant]
        type MinEffectivePrice: Get<u128>;

        /// Maximum effective price ceiling.
        #[pallet::constant]
        type MaxEffectivePrice: Get<u128>;

        /// Maximum absolute weight magnitude.
        #[pallet::constant]
        type MaxWeightMagnitude: Get<u128>;

        /// Minimum dynamic per-unit cap. Cap never falls below this.
        #[pallet::constant]
        type MinUnitCap: Get<i128>;

        /// Maximum dynamic per-unit cap.
        #[pallet::constant]
        type MaxUnitCap: Get<i128>;

        /// Default per-unit cap for newly registered fungible assets.
        #[pallet::constant]
        type DefaultUnitCap: Get<i128>;

        /// Maximum absolute unit slope in ppm.
        #[pallet::constant]
        type MaxUnitSlopePpm: Get<i128>;

        /// Scales distribution aggregate into effective-weight/price component, ppb.
        #[pallet::constant]
        type UnitDistWeightPpb: Get<u128>;

        /// Dynamic cap self-signal scale, ppb.
        #[pallet::constant]
        type UnitCapSelfPpb: Get<u128>;

        /// Dynamic cap inbound-signal scale, ppb.
        #[pallet::constant]
        type UnitCapInboundPpb: Get<u128>;

        /// Dynamic cap outbound-signal scale, ppb.
        #[pallet::constant]
        type UnitCapOutboundPpb: Get<u128>;

        /// Maximum absolute distribution aggregate magnitude.
        #[pallet::constant]
        type MaxDistributionAggregate: Get<i128>;
    }

    #[pallet::pallet]
    #[pallet::storage_version(STORAGE_VERSION)]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn asset_config)]
    pub type Assets<T: Config> = StorageMap<_, Blake2_128Concat, T::AccountId, AssetConfig<T>>;

    #[pallet::storage]
    #[pallet::getter(fn asset_state)]
    pub type AssetState<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        T::AccountId,
        AssetWeightState<BlockNumberFor<T>>,
        ValueQuery,
    >;

    #[pallet::storage]
    #[pallet::getter(fn asset_count)]
    pub type AssetCount<T> = StorageValue<_, u32, ValueQuery>;

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        AssetRegistered {
            contract: T::AccountId,
            oracle_id: OracleId,
            kind: AssetKind,
            last_price: u128,
            effective_price: u128,
        },
        WeightUpdated {
            contract: T::AccountId,
            base_weight: i128,
            effective_weight: i128,
        },
        PricePropagated {
            source: T::AccountId,
            old_base_weight: i128,
            new_base_weight: i128,
            source_old_effective_price: u128,
            source_new_effective_price: u128,
            total_budget: u128,
            absorbed: u128,
            unabsorbed: u128,
            affected_count: u32,
            bunches_touched: u32,
        },
        TransferRecorded {
            contract: T::AccountId,
            transfer_count: u64,
        },
        AssetDeregistered {
            contract: T::AccountId,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        AssetNotRegistered,
        AssetAlreadyRegistered,
        OracleNotFound,
        OracleInactive,
        NoLedgerPrice,
        InvalidPricePayload,
        PriceConflicted,
        ContractNotFound,
        NotAuthorized,
        WeightOverflow,
        InvalidAssetKind,
        TooManyAssets,
        DomainKeyTooLarge,
        InvalidDecimals,
        InvalidBandSize,
        InvalidDissimilarityScale,
        InvalidBunchCap,
        InvalidTotalBudgetCap,
        InvalidPriceBounds,
        InvalidUnitCap,
        InvalidUnitSlope,
        InvalidDistributionConfig,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Worst case: read config + state for source, read oracle, scan all assets (2 reads each),
        /// write source config? no, state, write up to MaxAssets target states, emit 2 events.
        #[pallet::call_index(0)]
        #[pallet::weight({
			let n = T::MaxAssets::get() as u64;
			Weight::from_parts(100_000_u64.saturating_add(n.saturating_mul(25_000)), 0)
				.saturating_add(T::DbWeight::get().reads(8_u64.saturating_add(n.saturating_mul(2))))
				.saturating_add(T::DbWeight::get().writes(4_u64.saturating_add(n)))
		})]
        pub fn register_asset(
            origin: OriginFor<T>,
            contract: T::AccountId,
            kind: AssetKind,
            oracle_id: OracleId,
            domain_key: Vec<u8>,
            decimals: u8,
            total_supply: u128,
        ) -> DispatchResult {
            let who = ensure_signed(origin.clone()).map_err(|_| Error::<T>::NotAuthorized)?;
            let is_admin = <T as pallet::Config>::AdminOrigin::ensure_origin(origin).is_ok();
            let owner = if is_admin { who.clone() } else { who };

            ensure!(
                !Assets::<T>::contains_key(&contract),
                Error::<T>::AssetAlreadyRegistered
            );
            ensure!(
                AssetCount::<T>::get() < T::MaxAssets::get(),
                Error::<T>::TooManyAssets
            );
            ensure!(decimals <= 18, Error::<T>::InvalidDecimals);
            ensure!(T::BandSize::get() > 0, Error::<T>::InvalidBandSize);
            ensure!(
                T::DissimilarityScale::get() > 0,
                Error::<T>::InvalidDissimilarityScale
            );
            ensure!(
                T::MaxBunchAbsorptionPerUpdate::get() > 0,
                Error::<T>::InvalidBunchCap
            );
            ensure!(
                T::MaxTotalCrossPriceDeltaPerUpdate::get() > 0,
                Error::<T>::InvalidTotalBudgetCap
            );
            ensure!(
                T::MinEffectivePrice::get() <= T::MaxEffectivePrice::get(),
                Error::<T>::InvalidPriceBounds
            );
            Self::validate_distribution_config()?;

            if !T::ContractInspector::contract_exists(&contract) {
                ensure!(is_admin, Error::<T>::ContractNotFound);
            }

            ensure!(
                T::OraclePrices::is_active_oracle(oracle_id),
                Error::<T>::OracleInactive
            );

            let domain_bounded: BoundedVec<u8, T::MaxDomainKeyLen> =
                BoundedVec::try_from(domain_key).map_err(|_| Error::<T>::DomainKeyTooLarge)?;
            let price = Self::read_oracle_price(oracle_id, domain_bounded.as_slice())?;

            let base_weight = Self::compute_base_weight(kind, total_supply, 0, price, decimals)?;
            let effective_price = Self::clamp_price(price);
            let unit_cap_w = Self::clamp_unit_cap(T::DefaultUnitCap::get());
            let unit_base_w = 0i128;
            let unit_slope_ppm = 0i128;
            let dist_aggregate = Self::compute_distribution_aggregate_o1(
                kind,
                total_supply,
                unit_base_w,
                unit_slope_ppm,
                unit_cap_w,
            )?;
            let effective_weight = Self::compute_effective_weight(base_weight, dist_aggregate);

            let now = <frame_system::Pallet<T>>::block_number();
            let config = AssetConfig::<T> {
                contract: contract.clone(),
                kind,
                oracle_id,
                domain_key: domain_bounded,
                decimals,
                registered_at: now,
                owner,
            };
            let state = AssetWeightState::<BlockNumberFor<T>> {
                last_price: price,
                effective_price,
                base_weight,
                effective_weight,
                total_supply,
                transfer_count: 0,
                last_update: now,
                unit_base_w,
                unit_slope_ppm,
                unit_cap_w,
                dist_aggregate,
                last_inbound_cross_w: 0,
                last_outbound_cross_w: 0,
            };

            Assets::<T>::insert(&contract, config);
            AssetState::<T>::insert(&contract, state);
            AssetCount::<T>::mutate(|c| *c = c.saturating_add(1));

            Self::deposit_event(Event::AssetRegistered {
                contract,
                oracle_id,
                kind,
                last_price: price,
                effective_price,
            });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight({
			let n = T::MaxAssets::get() as u64;
			// Reads: source config+state, oracle registry+ledger lookups approximated, all assets config+state.
			// Writes: source state, touched target states; worst case all assets touched.
			// Compute includes band grouping and bounded redistribution passes (<= MaxAssets bunches).
			Weight::from_parts(180_000_u64.saturating_add(n.saturating_mul(60_000)), 0)
				.saturating_add(T::DbWeight::get().reads(10_u64.saturating_add(n.saturating_mul(2))))
				.saturating_add(T::DbWeight::get().writes(3_u64.saturating_add(n)))
		})]
        pub fn refresh_weight(origin: OriginFor<T>, contract: T::AccountId) -> DispatchResult {
            let _ = ensure_signed(origin)?;
            let config = Assets::<T>::get(&contract).ok_or(Error::<T>::AssetNotRegistered)?;
            let mut state = AssetState::<T>::get(&contract);

            ensure!(T::BandSize::get() > 0, Error::<T>::InvalidBandSize);
            ensure!(
                T::DissimilarityScale::get() > 0,
                Error::<T>::InvalidDissimilarityScale
            );
            ensure!(
                T::MaxBunchAbsorptionPerUpdate::get() > 0,
                Error::<T>::InvalidBunchCap
            );
            ensure!(
                T::MaxTotalCrossPriceDeltaPerUpdate::get() > 0,
                Error::<T>::InvalidTotalBudgetCap
            );
            ensure!(
                T::MinEffectivePrice::get() <= T::MaxEffectivePrice::get(),
                Error::<T>::InvalidPriceBounds
            );

            let price = Self::read_oracle_price(config.oracle_id, config.domain_key.as_slice())?;
            let old_base_weight = state.base_weight;
            let old_effective_weight = state.effective_weight;
            let old_effective_price = state.effective_price;
            let old_unit_cap_w = state.unit_cap_w;

            let new_base_weight = Self::compute_base_weight(
                config.kind,
                state.total_supply,
                state.transfer_count,
                price,
                config.decimals,
            )?;

            // Recompute distribution under the current cap before self/outbound signals.
            let dist_aggregate = Self::compute_distribution_aggregate_o1(
                config.kind,
                state.total_supply,
                state.unit_base_w,
                state.unit_slope_ppm,
                old_unit_cap_w,
            )?;
            let new_effective_weight =
                Self::compute_effective_weight(new_base_weight, dist_aggregate);
            let delta_w = new_effective_weight.saturating_sub(old_effective_weight);

            let self_delta = Self::capped_price_delta(
                delta_w,
                price,
                T::SelfInfluencePpb::get(),
                T::MaxSelfPriceDeltaPerUpdate::get(),
            );
            let source_effective = Self::apply_price_delta(price, self_delta);

            let propagation =
                Self::propagate_price_impact(&contract, delta_w, new_base_weight, price)?;

            let outbound_signal = propagation.absorbed.min(i128::MAX as u128) as i128;
            let self_signal = self_delta.unsigned_abs().min(i128::MAX as u128) as i128;
            let unit_cap_w = Self::update_unit_cap(
                old_unit_cap_w,
                self_signal,
                0,
                outbound_signal,
            );
            let dist_aggregate = Self::compute_distribution_aggregate_o1(
                config.kind,
                state.total_supply,
                state.unit_base_w,
                state.unit_slope_ppm,
                unit_cap_w,
            )?;
            let effective_weight = Self::compute_effective_weight(new_base_weight, dist_aggregate);

            state.base_weight = new_base_weight;
            state.effective_weight = effective_weight;
            state.last_price = price;
            state.effective_price = source_effective;
            state.last_update = <frame_system::Pallet<T>>::block_number();
            state.unit_cap_w = unit_cap_w;
            state.dist_aggregate = dist_aggregate;
            state.last_inbound_cross_w = 0;
            state.last_outbound_cross_w = outbound_signal;
            AssetState::<T>::insert(&contract, state);

            Self::deposit_event(Event::WeightUpdated {
                contract: contract.clone(),
                base_weight: new_base_weight,
                effective_weight,
            });
            Self::deposit_event(Event::PricePropagated {
                source: contract,
                old_base_weight,
                new_base_weight,
                source_old_effective_price: old_effective_price,
                source_new_effective_price: source_effective,
                total_budget: propagation.total_budget,
                absorbed: propagation.absorbed,
                unabsorbed: propagation.unabsorbed,
                affected_count: propagation.affected_count,
                bunches_touched: propagation.bunches_touched,
            });
            Ok(())
        }

        /// Records an observed supply/transfer update for fungible and NFT assets.
        /// Recomputes distribution aggregate and effective weight; oracle prices are unchanged.
        #[pallet::call_index(2)]
        #[pallet::weight(Weight::from_parts(30_000, 0) + T::DbWeight::get().reads_writes(3, 2))]
        pub fn record_fungible_transfer(
            origin: OriginFor<T>,
            contract: T::AccountId,
            observed_total_supply: Option<u128>,
        ) -> DispatchResult {
            let who = ensure_signed(origin.clone())?;
            let config = Assets::<T>::get(&contract).ok_or(Error::<T>::AssetNotRegistered)?;
            let is_admin = <T as pallet::Config>::AdminOrigin::ensure_origin(origin).is_ok();
            ensure!(is_admin || who == config.owner, Error::<T>::NotAuthorized);

            AssetState::<T>::try_mutate(&contract, |state| -> DispatchResult {
                state.transfer_count = state.transfer_count.saturating_add(1);
                if let Some(supply) = observed_total_supply {
                    state.total_supply = supply;
                }
                let dist_aggregate = Self::compute_distribution_aggregate_o1(
                    config.kind,
                    state.total_supply,
                    state.unit_base_w,
                    state.unit_slope_ppm,
                    state.unit_cap_w,
                )?;
                state.dist_aggregate = dist_aggregate;
                state.effective_weight =
                    Self::compute_effective_weight(state.base_weight, dist_aggregate);
                state.last_update = <frame_system::Pallet<T>>::block_number();
                Self::deposit_event(Event::TransferRecorded {
                    contract: contract.clone(),
                    transfer_count: state.transfer_count,
                });
                Ok(())
            })
        }

        #[pallet::call_index(3)]
        #[pallet::weight(Weight::from_parts(40_000, 0) + T::DbWeight::get().reads_writes(4, 2))]
        pub fn update_asset_oracle(
            origin: OriginFor<T>,
            contract: T::AccountId,
            oracle_id: OracleId,
            domain_key: Vec<u8>,
        ) -> DispatchResult {
            let who = ensure_signed(origin.clone())?;
            let mut config = Assets::<T>::get(&contract).ok_or(Error::<T>::AssetNotRegistered)?;
            let is_admin = <T as pallet::Config>::AdminOrigin::ensure_origin(origin).is_ok();
            ensure!(is_admin || who == config.owner, Error::<T>::NotAuthorized);
            ensure!(
                T::OraclePrices::is_active_oracle(oracle_id),
                Error::<T>::OracleInactive
            );
            let domain_bounded: BoundedVec<u8, T::MaxDomainKeyLen> =
                BoundedVec::try_from(domain_key).map_err(|_| Error::<T>::DomainKeyTooLarge)?;
            // Ensure price is readable before binding.
            let _ = Self::read_oracle_price(oracle_id, domain_bounded.as_slice())?;
            config.oracle_id = oracle_id;
            config.domain_key = domain_bounded;
            Assets::<T>::insert(&contract, config);
            Ok(())
        }

        #[pallet::call_index(4)]
        #[pallet::weight(Weight::from_parts(30_000, 0) + T::DbWeight::get().reads_writes(2, 3))]
        pub fn deregister_asset(origin: OriginFor<T>, contract: T::AccountId) -> DispatchResult {
            let who = ensure_signed(origin.clone())?;
            let config = Assets::<T>::get(&contract).ok_or(Error::<T>::AssetNotRegistered)?;
            let is_admin = <T as pallet::Config>::AdminOrigin::ensure_origin(origin).is_ok();
            ensure!(is_admin || who == config.owner, Error::<T>::NotAuthorized);
            Assets::<T>::remove(&contract);
            AssetState::<T>::remove(&contract);
            AssetCount::<T>::mutate(|c| *c = c.saturating_sub(1));
            Self::deposit_event(Event::AssetDeregistered { contract });
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        const UNIT_SLOPE_PPM_SCALE: i128 = 1_000_000;
        const PPB_SCALE: u128 = 1_000_000_000;

        fn validate_distribution_config() -> DispatchResult {
            ensure!(
                T::MinUnitCap::get() > 0
                    && T::MinUnitCap::get() <= T::DefaultUnitCap::get()
                    && T::DefaultUnitCap::get() <= T::MaxUnitCap::get(),
                Error::<T>::InvalidUnitCap
            );
            ensure!(
                T::MaxUnitSlopePpm::get() >= 0,
                Error::<T>::InvalidUnitSlope
            );
            ensure!(
                T::MaxDistributionAggregate::get() >= 0,
                Error::<T>::InvalidDistributionConfig
            );
            Ok(())
        }

        fn read_oracle_price(
            oracle_id: OracleId,
            domain_key: &[u8],
        ) -> Result<u128, DispatchError> {
            match T::OraclePrices::latest_price(oracle_id, domain_key) {
                Ok(p) => Ok(p),
                Err(OracleReadError::OracleNotFound) => Err(Error::<T>::OracleNotFound.into()),
                Err(OracleReadError::OracleInactive) => Err(Error::<T>::OracleInactive.into()),
                Err(OracleReadError::NoPrice) => Err(Error::<T>::NoLedgerPrice.into()),
                Err(OracleReadError::DomainKeyTooLarge) => {
                    Err(Error::<T>::DomainKeyTooLarge.into())
                }
                Err(OracleReadError::InvalidPayload) => Err(Error::<T>::InvalidPricePayload.into()),
                Err(OracleReadError::Conflicted) => Err(Error::<T>::PriceConflicted.into()),
            }
        }

        pub fn compute_base_weight(
            kind: AssetKind,
            total_supply: u128,
            transfer_count: u64,
            price: u128,
            decimals: u8,
        ) -> Result<i128, DispatchError> {
            let max_mag = T::MaxWeightMagnitude::get() as i128;
            let supply_units = Self::scale_by_decimals(total_supply, decimals);
            let supply_component = Self::clamp_i128(supply_units as i128, max_mag);
            let transfer_component =
                Self::clamp_i128((transfer_count as i128).saturating_mul(1_000), max_mag);
            let price_units = Self::scale_by_decimals(price, decimals);
            let price_component = Self::clamp_i128(price_units as i128, max_mag);

            let raw = match kind {
                AssetKind::Fungible => supply_component
                    .saturating_add(transfer_component)
                    .saturating_sub(price_component),
                AssetKind::NonFungible => price_component
                    .saturating_sub(supply_component)
                    .saturating_sub(transfer_component),
            };
            Ok(Self::clamp_i128(raw, max_mag))
        }

        fn scale_by_decimals(value: u128, decimals: u8) -> u128 {
            let factor = 10u128.saturating_pow(decimals as u32);
            if factor == 0 {
                return value;
            }
            value / factor
        }

        fn clamp_i128(v: i128, max_mag: i128) -> i128 {
            v.clamp(-max_mag, max_mag)
        }

        fn clamp_unit_cap(cap: i128) -> i128 {
            cap.clamp(T::MinUnitCap::get(), T::MaxUnitCap::get())
        }

        #[allow(dead_code)]
        pub fn compute_unit_weight_raw(unit_base_w: i128, unit_slope_ppm: i128, i: u128) -> i128 {
            let slope_term = (unit_slope_ppm as i128)
                .saturating_mul(i.min(i128::MAX as u128) as i128)
                / Self::UNIT_SLOPE_PPM_SCALE;
            unit_base_w.saturating_add(slope_term)
        }

        pub fn clamp_unit_weight(w_raw: i128, unit_cap_w: i128) -> i128 {
            let cap = Self::clamp_unit_cap(unit_cap_w.max(0));
            w_raw.clamp(-cap, cap)
        }

    /// Deterministic O(1) aggregate over virtual supply ranks `i in [0, total_supply-1]`.
    /// For NFTs, "units" are virtual ranks over collection/contract supply, not token IDs.
    pub fn compute_distribution_aggregate_o1(
        _kind: AssetKind,
        total_supply: u128,
        unit_base_w: i128,
        unit_slope_ppm: i128,
        unit_cap_w: i128,
    ) -> Result<i128, DispatchError> {
        if total_supply == 0 {
            return Ok(0);
        }
            let n = total_supply.min(i128::MAX as u128) as i128;
            let cap = Self::clamp_unit_cap(unit_cap_w.max(0));
            let slope = unit_slope_ppm.clamp(0, T::MaxUnitSlopePpm::get());
            let base = Self::clamp_i128(unit_base_w, T::MaxWeightMagnitude::get() as i128);

            let raw = if slope == 0 {
                (Self::clamp_unit_weight(base, cap) as i128).saturating_mul(n)
            } else {
                // k_cap: first index where base + slope*i/scale >= cap
                let scale = Self::UNIT_SLOPE_PPM_SCALE;
                let cap_minus_base = cap.saturating_sub(base);
                let k_cap = if cap_minus_base <= 0 {
                    0i128
                } else {
                    // ceil(cap_minus_base * scale / slope)
                    let num = (cap_minus_base as i128).saturating_mul(scale);
                    (num.saturating_add(slope - 1)) / slope
                };
                let k = k_cap.clamp(0, n);
                // sum_{i=0}^{k-1} w_raw(i) = k*base + slope/scale * (k-1)*k/2
                let k_u = k as u128;
                let tri = k_u.saturating_mul(k_u.saturating_sub(1)) / 2;
                let slope_sum = (slope as i128).saturating_mul(tri.min(i128::MAX as u128) as i128)
                    / scale;
                let uncapped_sum = (base.saturating_mul(k)).saturating_add(slope_sum);
                let capped_count = n.saturating_sub(k);
                uncapped_sum.saturating_add(cap.saturating_mul(capped_count))
            };

            Ok(Self::clamp_i128(raw, T::MaxDistributionAggregate::get()))
        }

        fn scale_distribution_component(dist_aggregate: i128) -> i128 {
            let ppb = T::UnitDistWeightPpb::get();
            if ppb == 0 || dist_aggregate == 0 {
                return 0;
            }
            let mag = dist_aggregate.unsigned_abs();
            let scaled = mag.saturating_mul(ppb) / Self::PPB_SCALE;
            let signed = if dist_aggregate >= 0 {
                scaled.min(i128::MAX as u128) as i128
            } else {
                -((scaled.min(i128::MAX as u128)) as i128)
            };
            Self::clamp_i128(signed, T::MaxWeightMagnitude::get() as i128)
        }

        fn compute_effective_weight(base_weight: i128, dist_aggregate: i128) -> i128 {
            let dist_component = Self::scale_distribution_component(dist_aggregate);
            Self::clamp_i128(
                base_weight.saturating_add(dist_component),
                T::MaxWeightMagnitude::get() as i128,
            )
        }

        fn cap_delta_from_signal(signal: i128, ppb: u128) -> i128 {
            if signal == 0 || ppb == 0 {
                return 0;
            }
            let mag = signal.unsigned_abs().saturating_mul(ppb) / Self::PPB_SCALE;
            let mag = mag.min(i128::MAX as u128) as i128;
            if signal >= 0 { mag } else { -mag }
        }

        pub fn update_unit_cap(
            current_cap: i128,
            self_signal: i128,
            inbound_signal: i128,
            outbound_signal: i128,
        ) -> i128 {
            let delta = Self::cap_delta_from_signal(self_signal, T::UnitCapSelfPpb::get())
                .saturating_add(Self::cap_delta_from_signal(
                    inbound_signal,
                    T::UnitCapInboundPpb::get(),
                ))
                .saturating_sub(Self::cap_delta_from_signal(
                    outbound_signal,
                    T::UnitCapOutboundPpb::get(),
                ));
            Self::clamp_unit_cap(current_cap.saturating_add(delta))
        }

        fn clamp_price(price: u128) -> u128 {
            price.clamp(T::MinEffectivePrice::get(), T::MaxEffectivePrice::get())
        }

        fn apply_price_delta(base: u128, delta: i128) -> u128 {
            let next = if delta >= 0 {
                base.saturating_add(delta as u128)
            } else {
                base.saturating_sub(delta.unsigned_abs())
            };
            Self::clamp_price(next)
        }

        fn capped_price_delta(delta_w: i128, _anchor_price: u128, ppb: u128, cap: u128) -> i128 {
            if delta_w == 0 || ppb == 0 {
                return 0;
            }
            let mag = delta_w.unsigned_abs();
            // Deterministic fixed-point scaling: delta_w (weight units) * ppb / 1e9 => price units.
            let scaled = mag.saturating_mul(ppb) / 1_000_000_000u128;
            let capped = scaled.min(cap);
            if delta_w > 0 {
                capped as i128
            } else {
                -(capped as i128)
            }
        }

        fn band_index(weight: i128, band_size: u128) -> i128 {
            let size = band_size.max(1) as i128;
            // Deterministic floor division for signed weights.
            if weight >= 0 {
                weight / size
            } else {
                -(((-weight) + size - 1) / size)
            }
        }

        fn dissimilarity_score(distance: u128) -> u128 {
            distance.min(T::DissimilarityScale::get())
        }

        fn price_headroom(current: u128, sign_positive: bool) -> u128 {
            if sign_positive {
                T::MaxEffectivePrice::get().saturating_sub(current)
            } else {
                current.saturating_sub(T::MinEffectivePrice::get())
            }
        }

        fn propagate_price_impact(
            source: &T::AccountId,
            delta_w: i128,
            new_source_base_weight: i128,
            source_price: u128,
        ) -> Result<PropagationResult, DispatchError> {
            let zero = PropagationResult::default();
            if delta_w == 0 {
                return Ok(zero);
            }

            let total_budget_signed = Self::capped_price_delta(
                delta_w,
                source_price.max(1),
                T::CrossInfluencePpb::get(),
                T::MaxTotalCrossPriceDeltaPerUpdate::get(),
            );
            if total_budget_signed == 0 {
                return Ok(zero);
            }
            let total_budget_mag = total_budget_signed.unsigned_abs();
            let sign_positive = total_budget_signed > 0;

            let mut bunches: BTreeMap<i128, Bunch<T>> = BTreeMap::new();
            let mut epsilon_targets: Vec<TargetMember<T>> = Vec::new();

            for (contract, _config) in Assets::<T>::iter() {
                if &contract == source {
                    continue;
                }
                let state = AssetState::<T>::get(&contract);
                let mut state = state;
                state.last_inbound_cross_w = 0;
                let distance = new_source_base_weight.abs_diff(state.base_weight);
                let score = Self::dissimilarity_score(distance);
                let target = TargetMember {
                    contract,
                    state,
                    dissimilarity: score,
                    applied_mag: 0,
                    exhausted: false,
                };
                if score == 0 {
                    epsilon_targets.push(target);
                } else {
                    let band = Self::band_index(target.state.base_weight, T::BandSize::get());
                    bunches
                        .entry(band)
                        .or_insert_with(Bunch::new)
                        .members
                        .push(target);
                }
            }

            for bunch in bunches.values_mut() {
                bunch.recompute_weight();
            }

            let mut affected_contracts: BTreeMap<
                T::AccountId,
                AssetWeightState<BlockNumberFor<T>>,
            > = BTreeMap::new();
            let mut absorbed_mag: u128 = 0;
            let mut touched_bands: BTreeMap<i128, bool> = BTreeMap::new();
            let mut epsilon_touched = false;
            let mut remaining = total_budget_mag;
            let max_passes = T::MaxAssets::get().max(1);

            for _ in 0..max_passes {
                if remaining == 0 {
                    break;
                }
                let active_total_weight: u128 = bunches
                    .values()
                    .filter(|b| b.is_active())
                    .map(|b| b.total_weight)
                    .fold(0u128, |acc, w| acc.saturating_add(w));
                if active_total_weight == 0 {
                    break;
                }

                let mut desired: Vec<(i128, u128)> = Vec::new();
                for (&band, bunch) in bunches.iter() {
                    if !bunch.is_active() {
                        continue;
                    }
                    let share = remaining.saturating_mul(bunch.total_weight) / active_total_weight;
                    if share > 0 {
                        desired.push((band, share));
                    }
                }
                if desired.is_empty() {
                    // Deterministic dust: assign one unit to highest-weight active bunch with capacity.
                    if let Some((&band, _)) =
                        bunches
                            .iter()
                            .filter(|(_, b)| b.is_active())
                            .max_by(|(ba, a), (bb, b)| {
                                a.total_weight.cmp(&b.total_weight).then_with(|| ba.cmp(bb))
                            })
                    {
                        desired.push((band, remaining.min(1)));
                    } else {
                        break;
                    }
                }

                let mut pass_absorbed: u128 = 0;
                for (band, mut want) in desired {
                    if remaining == 0 {
                        break;
                    }
                    want = want.min(remaining);
                    if want == 0 {
                        continue;
                    }
                    let Some(bunch) = bunches.get_mut(&band) else {
                        continue;
                    };
                    let cap_remaining =
                        T::MaxBunchAbsorptionPerUpdate::get().saturating_sub(bunch.absorbed_mag);
                    if cap_remaining == 0 {
                        bunch.saturated = true;
                        continue;
                    }
                    want = want.min(cap_remaining);
                    let applied =
                        Self::apply_to_bunch(bunch, want, sign_positive, &mut affected_contracts);
                    if applied > 0 {
                        bunch.absorbed_mag = bunch.absorbed_mag.saturating_add(applied);
                        pass_absorbed = pass_absorbed.saturating_add(applied);
                        absorbed_mag = absorbed_mag.saturating_add(applied);
                        touched_bands.insert(band, true);
                        if bunch.absorbed_mag >= T::MaxBunchAbsorptionPerUpdate::get() {
                            bunch.saturated = true;
                        }
                    } else {
                        bunch.saturated = true;
                    }
                }

                if pass_absorbed == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(pass_absorbed);
            }

            // Epsilon residual pass: d==0 targets only after dissimilar capacity/budget is exhausted.
            if remaining > 0 && !epsilon_targets.is_empty() {
                let mut epsilon_bunch = Bunch::<T>::new();
                epsilon_bunch.members = epsilon_targets;
                for m in epsilon_bunch.members.iter_mut() {
                    // Minimal epsilon weight, never larger than any d>0 target.
                    m.dissimilarity = 1;
                    m.exhausted = false;
                }
                epsilon_bunch.recompute_weight();
                let want = remaining.min(T::MaxBunchAbsorptionPerUpdate::get());
                let applied = Self::apply_to_bunch(
                    &mut epsilon_bunch,
                    want,
                    sign_positive,
                    &mut affected_contracts,
                );
                if applied > 0 {
                    absorbed_mag = absorbed_mag.saturating_add(applied);
                    epsilon_touched = true;
                    remaining = remaining.saturating_sub(applied);
                }
            }

            let now = <frame_system::Pallet<T>>::block_number();
            let mut affected_count: u32 = 0;
            for (contract, mut state) in affected_contracts {
                // Inbound cross signal: absorbed price-delta magnitude applied to this target.
                let inbound_mag = state.last_inbound_cross_w.saturating_abs();
                let unit_cap_w = Self::update_unit_cap(state.unit_cap_w, 0, inbound_mag, 0);
                state.unit_cap_w = unit_cap_w;
                let dist_aggregate = Self::compute_distribution_aggregate_o1(
                    Assets::<T>::get(&contract)
                        .map(|c| c.kind)
                        .unwrap_or(AssetKind::Fungible),
                    state.total_supply,
                    state.unit_base_w,
                    state.unit_slope_ppm,
                    unit_cap_w,
                )?;
                state.dist_aggregate = dist_aggregate;
                state.effective_weight =
                    Self::compute_effective_weight(state.base_weight, dist_aggregate);
                state.last_update = now;
                AssetState::<T>::insert(&contract, state);
                affected_count = affected_count.saturating_add(1);
            }

            let bunches_touched =
                touched_bands
                    .len()
                    .saturating_add(if epsilon_touched { 1 } else { 0 }) as u32;
            Ok(PropagationResult {
                total_budget: total_budget_mag,
                absorbed: absorbed_mag,
                unabsorbed: remaining,
                affected_count,
                bunches_touched,
                source_inbound_absorbed: 0,
            })
        }

        fn apply_to_bunch(
            bunch: &mut Bunch<T>,
            desired_mag: u128,
            sign_positive: bool,
            affected: &mut BTreeMap<T::AccountId, AssetWeightState<BlockNumberFor<T>>>,
        ) -> u128 {
            if desired_mag == 0 || !bunch.is_active() {
                return 0;
            }
            let total_weight = bunch.active_weight();
            if total_weight == 0 {
                bunch.saturated = true;
                return 0;
            }

            let mut applied_mag: u128 = 0;
            let per_asset_cap = T::MaxCrossPriceDeltaPerUpdate::get();
            for member in bunch.members.iter_mut() {
                if member.exhausted || member.dissimilarity == 0 {
                    continue;
                }
                let share = desired_mag.saturating_mul(member.dissimilarity) / total_weight;
                if share == 0 {
                    continue;
                }
                let share = share.min(per_asset_cap);
                let room = Self::price_headroom(member.state.effective_price, sign_positive);
                if room == 0 {
                    member.exhausted = true;
                    continue;
                }
                let actual = share.min(room).min(per_asset_cap);
                if actual == 0 {
                    member.exhausted = true;
                    continue;
                }
                let signed = if sign_positive {
                    actual as i128
                } else {
                    -(actual as i128)
                };
                let new_price = Self::apply_price_delta(member.state.effective_price, signed);
                if new_price == member.state.effective_price {
                    member.exhausted = true;
                    continue;
                }
                let actual_applied = new_price.abs_diff(member.state.effective_price);
                member.state.effective_price = new_price;
                member.state.last_inbound_cross_w =
                    member.state.last_inbound_cross_w.saturating_add(actual_applied.min(i128::MAX as u128) as i128);
                member.applied_mag = member.applied_mag.saturating_add(actual);
                if member.applied_mag >= per_asset_cap
                    || Self::price_headroom(member.state.effective_price, sign_positive) == 0
                {
                    member.exhausted = true;
                }
                applied_mag = applied_mag.saturating_add(actual_applied);
                affected.insert(member.contract.clone(), member.state);
            }

            if bunch.active_weight() == 0 {
                bunch.saturated = true;
            }
            applied_mag
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, RuntimeDebug)]
struct PropagationResult {
    total_budget: u128,
    absorbed: u128,
    unabsorbed: u128,
    affected_count: u32,
    bunches_touched: u32,
    source_inbound_absorbed: u128,
}

struct TargetMember<T: Config> {
    contract: T::AccountId,
    state: AssetWeightState<BlockNumberFor<T>>,
    dissimilarity: u128,
    applied_mag: u128,
    exhausted: bool,
}

struct Bunch<T: Config> {
    members: Vec<TargetMember<T>>,
    total_weight: u128,
    absorbed_mag: u128,
    saturated: bool,
}

impl<T: Config> Bunch<T> {
    fn new() -> Self {
        Self {
            members: Vec::new(),
            total_weight: 0,
            absorbed_mag: 0,
            saturated: false,
        }
    }

    fn recompute_weight(&mut self) {
        self.total_weight = self
            .members
            .iter()
            .filter(|m| !m.exhausted)
            .map(|m| m.dissimilarity)
            .fold(0u128, |acc, w| acc.saturating_add(w));
    }

    fn active_weight(&self) -> u128 {
        self.members
            .iter()
            .filter(|m| !m.exhausted && m.dissimilarity > 0)
            .map(|m| m.dissimilarity)
            .fold(0u128, |acc, w| acc.saturating_add(w))
    }

    fn is_active(&self) -> bool {
        !self.saturated
            && self.absorbed_mag < T::MaxBunchAbsorptionPerUpdate::get()
            && self.active_weight() > 0
    }
}

/// Runtime adapter reading prices from oracle-registry/oracle-ledger storage.
///
/// Payload format: SCALE-encoded `u128`. Conflicted latest domain entries are rejected.
pub struct RegistryLedgerOraclePriceProvider<T>(core::marker::PhantomData<T>);

impl<T> OraclePriceProvider<T::MaxDomainKeyLen> for RegistryLedgerOraclePriceProvider<T>
where
    T: registry::Config + ledger::Config,
{
    fn is_active_oracle(oracle_id: OracleId) -> bool {
        registry::Pallet::<T>::get_oracle(oracle_id)
            .map(|o| o.status == OracleStatus::Active)
            .unwrap_or(false)
    }

    fn latest_price(oracle_id: OracleId, domain_key: &[u8]) -> Result<u128, OracleReadError> {
        let domain: BoundedVec<u8, T::MaxDomainKeyLen> = BoundedVec::try_from(domain_key.to_vec())
            .map_err(|_| OracleReadError::DomainKeyTooLarge)?;
        let oracle =
            registry::Pallet::<T>::get_oracle(oracle_id).ok_or(OracleReadError::OracleNotFound)?;
        if oracle.status != OracleStatus::Active {
            return Err(OracleReadError::OracleInactive);
        }
        let entry_id = ledger::LastSeenByOracleDomain::<T>::get((oracle_id, domain))
            .ok_or(OracleReadError::NoPrice)?;
        let entry = ledger::LedgerEntries::<T>::get(entry_id).ok_or(OracleReadError::NoPrice)?;
        if entry.outcome == ledger::VerificationOutcome::ConflictDetected {
            return Err(OracleReadError::Conflicted);
        }
        u128::decode(&mut &entry.payload[..]).map_err(|_| OracleReadError::InvalidPayload)
    }
}
