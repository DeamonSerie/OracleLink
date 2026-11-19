#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;
    use sp_runtime::RuntimeDebug;
    use sp_std::vec::Vec;

    pub type OracleId = u64;
    pub type ChainId = u64;

    #[derive(
        Clone, Encode, Decode, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen, Default,
    )]
    pub enum OracleStatus {
        #[default]
        Active,
        Paused,
    }

    #[derive(Clone, Encode, Decode, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
    #[scale_info(skip_type_params(T))]
    pub struct OracleConfig<T: Config> {
        pub source_chain_id: ChainId,
        pub name: BoundedVec<u8, T::MaxOracleNameLen>,
        pub metadata: BoundedVec<u8, T::MaxMetadataLen>,
        pub status: OracleStatus,
        pub threshold: u32,
        pub signer_count: u32,
        pub created_at: BlockNumberFor<T>,
        pub next_nonce: u64,
    }

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

        type AdminOrigin: EnsureOrigin<Self::RuntimeOrigin>;

        #[pallet::constant]
        type MaxSigners: Get<u32>;
        #[pallet::constant]
        type MaxOracleNameLen: Get<u32>;
        #[pallet::constant]
        type MaxMetadataLen: Get<u32>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn next_oracle_id)]
    pub type NextOracleId<T> = StorageValue<_, OracleId, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn oracles)]
    pub type Oracles<T: Config> = StorageMap<_, Blake2_128Concat, OracleId, OracleConfig<T>>;

    #[pallet::storage]
    #[pallet::getter(fn signers)]
    pub type AuthorizedSigners<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        OracleId,
        BoundedVec<sp_runtime::MultiSigner, T::MaxSigners>,
        ValueQuery,
    >;

    #[pallet::storage]
    pub type OracleByChainName<T: Config> =
        StorageMap<_, Blake2_128Concat, (ChainId, BoundedVec<u8, T::MaxOracleNameLen>), OracleId>;

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        OracleRegistered {
            oracle_id: OracleId,
            source_chain_id: ChainId,
        },
        SignersUpdated {
            oracle_id: OracleId,
            signer_count: u32,
            threshold: u32,
        },
        OraclePaused {
            oracle_id: OracleId,
        },
        OracleResumed {
            oracle_id: OracleId,
        },
        NonceAdvanced {
            oracle_id: OracleId,
            nonce: u64,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        OracleNotFound,
        DuplicateOracle,
        InvalidThreshold,
        SignerSetTooLarge,
        SignerSetEmpty,
        DuplicateSigner,
        OracleInactive,
        BadNonce,
        NameTooLong,
        MetadataTooLong,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(3, 4))]
        pub fn register_oracle(
            origin: OriginFor<T>,
            source_chain_id: ChainId,
            name: Vec<u8>,
            metadata: Vec<u8>,
            signers: Vec<sp_runtime::MultiSigner>,
            threshold: u32,
        ) -> DispatchResult {
            T::AdminOrigin::ensure_origin(origin)?;
            let name_bounded: BoundedVec<u8, T::MaxOracleNameLen> =
                BoundedVec::try_from(name).map_err(|_| Error::<T>::NameTooLong)?;
            let metadata_bounded: BoundedVec<u8, T::MaxMetadataLen> =
                BoundedVec::try_from(metadata).map_err(|_| Error::<T>::MetadataTooLong)?;
            ensure!(
                !OracleByChainName::<T>::contains_key((source_chain_id, name_bounded.clone())),
                Error::<T>::DuplicateOracle
            );
            let signer_bounded = Self::validate_signers(signers, threshold)?;
            let oracle_id = NextOracleId::<T>::get();
            let next_id = oracle_id
                .checked_add(1)
                .ok_or(Error::<T>::DuplicateOracle)?;
            let config = OracleConfig::<T> {
                source_chain_id,
                name: name_bounded.clone(),
                metadata: metadata_bounded,
                status: OracleStatus::Active,
                threshold,
                signer_count: signer_bounded.len() as u32,
                created_at: <frame_system::Pallet<T>>::block_number(),
                next_nonce: 0,
            };
            NextOracleId::<T>::put(next_id);
            Oracles::<T>::insert(oracle_id, config);
            AuthorizedSigners::<T>::insert(oracle_id, signer_bounded);
            OracleByChainName::<T>::insert((source_chain_id, name_bounded), oracle_id);
            Self::deposit_event(Event::OracleRegistered {
                oracle_id,
                source_chain_id,
            });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(1, 2))]
        pub fn update_signers(
            origin: OriginFor<T>,
            oracle_id: OracleId,
            signers: Vec<sp_runtime::MultiSigner>,
            threshold: u32,
        ) -> DispatchResult {
            T::AdminOrigin::ensure_origin(origin)?;
            Oracles::<T>::try_mutate(oracle_id, |maybe| -> DispatchResult {
                let cfg = maybe.as_mut().ok_or(Error::<T>::OracleNotFound)?;
                let signer_bounded = Self::validate_signers(signers, threshold)?;
                cfg.threshold = threshold;
                cfg.signer_count = signer_bounded.len() as u32;
                AuthorizedSigners::<T>::insert(oracle_id, signer_bounded);
                Self::deposit_event(Event::SignersUpdated {
                    oracle_id,
                    signer_count: cfg.signer_count,
                    threshold,
                });
                Ok(())
            })
        }

        #[pallet::call_index(2)]
        #[pallet::weight(Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(1, 2))]
        pub fn rotate_keys(
            origin: OriginFor<T>,
            oracle_id: OracleId,
            signers: Vec<sp_runtime::MultiSigner>,
            threshold: u32,
        ) -> DispatchResult {
            Self::update_signers(origin, oracle_id, signers, threshold)
        }

        #[pallet::call_index(3)]
        #[pallet::weight(Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(2, 1))]
        pub fn set_threshold(
            origin: OriginFor<T>,
            oracle_id: OracleId,
            threshold: u32,
        ) -> DispatchResult {
            T::AdminOrigin::ensure_origin(origin)?;
            let signers = AuthorizedSigners::<T>::get(oracle_id);
            ensure!(!signers.is_empty(), Error::<T>::SignerSetEmpty);
            ensure!(
                threshold > 0 && threshold <= signers.len() as u32,
                Error::<T>::InvalidThreshold
            );
            Oracles::<T>::try_mutate(oracle_id, |maybe| -> DispatchResult {
                let cfg = maybe.as_mut().ok_or(Error::<T>::OracleNotFound)?;
                cfg.threshold = threshold;
                Self::deposit_event(Event::SignersUpdated {
                    oracle_id,
                    signer_count: cfg.signer_count,
                    threshold,
                });
                Ok(())
            })
        }

        #[pallet::call_index(4)]
        #[pallet::weight(Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(1, 1))]
        pub fn pause_oracle(origin: OriginFor<T>, oracle_id: OracleId) -> DispatchResult {
            T::AdminOrigin::ensure_origin(origin)?;
            Oracles::<T>::try_mutate(oracle_id, |maybe| -> DispatchResult {
                let cfg = maybe.as_mut().ok_or(Error::<T>::OracleNotFound)?;
                cfg.status = OracleStatus::Paused;
                Self::deposit_event(Event::OraclePaused { oracle_id });
                Ok(())
            })
        }

        #[pallet::call_index(5)]
        #[pallet::weight(Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(1, 1))]
        pub fn resume_oracle(origin: OriginFor<T>, oracle_id: OracleId) -> DispatchResult {
            T::AdminOrigin::ensure_origin(origin)?;
            Oracles::<T>::try_mutate(oracle_id, |maybe| -> DispatchResult {
                let cfg = maybe.as_mut().ok_or(Error::<T>::OracleNotFound)?;
                cfg.status = OracleStatus::Active;
                Self::deposit_event(Event::OracleResumed { oracle_id });
                Ok(())
            })
        }
    }

    impl<T: Config> Pallet<T> {
        fn validate_signers(
            signers: Vec<sp_runtime::MultiSigner>,
            threshold: u32,
        ) -> Result<BoundedVec<sp_runtime::MultiSigner, T::MaxSigners>, DispatchError> {
            ensure!(!signers.is_empty(), Error::<T>::SignerSetEmpty);
            ensure!(
                signers.len() as u32 <= T::MaxSigners::get(),
                Error::<T>::SignerSetTooLarge
            );
            ensure!(
                threshold > 0 && threshold <= signers.len() as u32,
                Error::<T>::InvalidThreshold
            );
            for (i, s) in signers.iter().enumerate() {
                for s2 in signers.iter().skip(i + 1) {
                    ensure!(s != s2, Error::<T>::DuplicateSigner);
                }
            }
            BoundedVec::try_from(signers).map_err(|_| Error::<T>::SignerSetTooLarge.into())
        }

        pub fn consume_nonce(oracle_id: OracleId, nonce: u64) -> DispatchResult {
            Oracles::<T>::try_mutate(oracle_id, |maybe| -> DispatchResult {
                let cfg = maybe.as_mut().ok_or(Error::<T>::OracleNotFound)?;
                ensure!(
                    cfg.status == OracleStatus::Active,
                    Error::<T>::OracleInactive
                );
                ensure!(cfg.next_nonce == nonce, Error::<T>::BadNonce);
                cfg.next_nonce = cfg.next_nonce.saturating_add(1);
                Self::deposit_event(Event::NonceAdvanced { oracle_id, nonce });
                Ok(())
            })
        }

        pub fn get_oracle(oracle_id: OracleId) -> Option<OracleConfig<T>> {
            Oracles::<T>::get(oracle_id)
        }

        pub fn get_signers(
            oracle_id: OracleId,
        ) -> BoundedVec<sp_runtime::MultiSigner, T::MaxSigners> {
            AuthorizedSigners::<T>::get(oracle_id)
        }
    }
}
