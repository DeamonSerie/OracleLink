#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;
    use pallet_oracle_registry::{self as registry, OracleId};
    use sp_io::hashing::blake2_256;
    use sp_runtime::traits::Verify;
    use sp_runtime::{MultiSignature, RuntimeDebug};
    use sp_std::vec::Vec;

    pub type ChainId = u64;
    pub type EntryId = u64;

    #[derive(
        Clone, Encode, Decode, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen, Default,
    )]
    pub enum VerificationOutcome {
        #[default]
        New,
        ConsistentWithHistory,
        ConflictDetected,
        Supersedes,
    }

    #[derive(Clone, Encode, Decode, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
    #[scale_info(skip_type_params(T))]
    pub struct ExternalTxRef<T: Config> {
        pub source_chain_id: ChainId,
        pub tx_hash: BoundedVec<u8, T::MaxTxRefLen>,
        pub block_ref: BoundedVec<u8, T::MaxTxRefLen>,
    }

    #[derive(Clone, Encode, Decode, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
    pub struct LinkedOracleLinkTx {
        pub block_number: u64,
        pub extrinsic_index: u32,
    }

    #[derive(Clone, Encode, Decode, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
    #[scale_info(skip_type_params(T))]
    pub struct LedgerEntry<T: Config> {
        pub entry_id: EntryId,
        pub oracle_id: OracleId,
        pub source_chain_id: ChainId,
        pub domain_key: BoundedVec<u8, T::MaxDomainKeyLen>,
        pub payload_hash: [u8; 32],
        pub payload: BoundedVec<u8, T::MaxPayloadLen>,
        pub outcome: VerificationOutcome,
        pub linked_oraclelink_tx: LinkedOracleLinkTx,
        pub linked_external_txs: BoundedVec<ExternalTxRef<T>, T::MaxExternalTxRefs>,
        pub submitted_by: T::AccountId,
        pub block_number: BlockNumberFor<T>,
        pub entry_hash: [u8; 32],
        pub accumulator_root: [u8; 32],
    }

    #[pallet::config]
    pub trait Config: frame_system::Config + registry::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

        #[pallet::constant]
        type MaxPayloadLen: Get<u32>;
        #[pallet::constant]
        type MaxDomainKeyLen: Get<u32>;
        #[pallet::constant]
        type MaxTxRefLen: Get<u32>;
        #[pallet::constant]
        type MaxExternalTxRefs: Get<u32>;
        #[pallet::constant]
        type MaxSignatures: Get<u32>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn next_entry_id)]
    pub type NextEntryId<T> = StorageValue<_, EntryId, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn latest_root)]
    pub type LatestAccumulatorRoot<T> = StorageValue<_, [u8; 32], ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn entries)]
    pub type LedgerEntries<T: Config> = StorageMap<_, Blake2_128Concat, EntryId, LedgerEntry<T>>;

    #[pallet::storage]
    pub type LastSeenByOracleDomain<T: Config> =
        StorageMap<_, Blake2_128Concat, (OracleId, BoundedVec<u8, T::MaxDomainKeyLen>), EntryId>;

    #[pallet::storage]
    pub type SeenPayload<T: Config> =
        StorageMap<_, Blake2_128Concat, (OracleId, [u8; 32]), EntryId>;

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        DataAppended {
            entry_id: EntryId,
            oracle_id: OracleId,
            outcome: VerificationOutcome,
        },
        VerificationRecorded {
            entry_id: EntryId,
            accumulator_root: [u8; 32],
        },
        ConflictDetected {
            entry_id: EntryId,
            oracle_id: OracleId,
            prior_entry_id: EntryId,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        OracleInactive,
        BadSignature,
        ThresholdNotMet,
        Replay,
        HistoryCheckFailed,
        TooManyExternalTxLinks,
        PayloadTooLarge,
        DomainKeyTooLarge,
        TxRefTooLarge,
        TooManySignatures,
        DuplicateSignerInProof,
        SignerNotAuthorized,
        EntryOverflow,
        OracleError,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(Weight::from_parts(50_000, 0) + T::DbWeight::get().reads_writes(8, 6))]
        pub fn submit_oracle_data(
            origin: OriginFor<T>,
            oracle_id: OracleId,
            source_chain_id: ChainId,
            domain_key: Vec<u8>,
            payload: Vec<u8>,
            source_tx_refs: Vec<(u64, Vec<u8>, Vec<u8>)>,
            nonce: u64,
            signatures: Vec<(sp_runtime::MultiSigner, MultiSignature)>,
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;

            registry::Pallet::<T>::consume_nonce(oracle_id, nonce)
                .map_err(|_| Error::<T>::OracleError)?;
            let oracle =
                registry::Pallet::<T>::get_oracle(oracle_id).ok_or(Error::<T>::OracleError)?;
            ensure!(
                oracle.source_chain_id == source_chain_id,
                Error::<T>::OracleError
            );

            let domain_bounded: BoundedVec<u8, T::MaxDomainKeyLen> =
                BoundedVec::try_from(domain_key).map_err(|_| Error::<T>::DomainKeyTooLarge)?;
            let payload_bounded: BoundedVec<u8, T::MaxPayloadLen> =
                BoundedVec::try_from(payload.clone()).map_err(|_| Error::<T>::PayloadTooLarge)?;

            ensure!(
                source_tx_refs.len() as u32 <= T::MaxExternalTxRefs::get(),
                Error::<T>::TooManyExternalTxLinks
            );
            let mut ext_refs: BoundedVec<ExternalTxRef<T>, T::MaxExternalTxRefs> =
                BoundedVec::default();
            for (scid, hash, bref) in source_tx_refs.into_iter() {
                let tx_hash = BoundedVec::try_from(hash).map_err(|_| Error::<T>::TxRefTooLarge)?;
                let block_ref =
                    BoundedVec::try_from(bref).map_err(|_| Error::<T>::TxRefTooLarge)?;
                ext_refs
                    .try_push(ExternalTxRef {
                        source_chain_id: scid,
                        tx_hash,
                        block_ref,
                    })
                    .map_err(|_| Error::<T>::TooManyExternalTxLinks)?;
            }

            let payload_hash = blake2_256(&payload);
            ensure!(
                !SeenPayload::<T>::contains_key((oracle_id, payload_hash)),
                Error::<T>::Replay
            );

            let message = Self::signing_message(
                oracle_id,
                source_chain_id,
                &domain_bounded,
                &payload_hash,
                nonce,
            );
            Self::verify_threshold(oracle_id, &message, &signatures)?;

            let (outcome, conflict_prior) =
                Self::historical_check(oracle_id, &domain_bounded, payload_hash);

            let entry_id = NextEntryId::<T>::get();
            let next_id = entry_id.checked_add(1).ok_or(Error::<T>::EntryOverflow)?;

            let block_number = <frame_system::Pallet<T>>::block_number();
            let ext_index = <frame_system::Pallet<T>>::extrinsic_index().unwrap_or(0);
            let bn_u64: u64 = block_number.try_into().unwrap_or(0u64);

            let prev_root = LatestAccumulatorRoot::<T>::get();
            let entry_hash = Self::entry_hash(
                entry_id,
                oracle_id,
                &domain_bounded,
                &payload_hash,
                &prev_root,
            );
            let accumulator_root = Self::accumulator_root(&prev_root, &entry_hash);

            let entry = LedgerEntry::<T> {
                entry_id,
                oracle_id,
                source_chain_id,
                domain_key: domain_bounded.clone(),
                payload_hash,
                payload: payload_bounded,
                outcome: outcome.clone(),
                linked_oraclelink_tx: LinkedOracleLinkTx {
                    block_number: bn_u64,
                    extrinsic_index: ext_index,
                },
                linked_external_txs: ext_refs,
                submitted_by: who,
                block_number,
                entry_hash,
                accumulator_root,
            };

            NextEntryId::<T>::put(next_id);
            LedgerEntries::<T>::insert(entry_id, entry);
            SeenPayload::<T>::insert((oracle_id, payload_hash), entry_id);
            LastSeenByOracleDomain::<T>::insert((oracle_id, domain_bounded), entry_id);
            LatestAccumulatorRoot::<T>::put(accumulator_root);

            if let Some(prior) = conflict_prior {
                Self::deposit_event(Event::ConflictDetected {
                    entry_id,
                    oracle_id,
                    prior_entry_id: prior,
                });
            }
            Self::deposit_event(Event::DataAppended {
                entry_id,
                oracle_id,
                outcome,
            });
            Self::deposit_event(Event::VerificationRecorded {
                entry_id,
                accumulator_root,
            });
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        fn signing_message(
            oracle_id: OracleId,
            source_chain_id: ChainId,
            domain_key: &BoundedVec<u8, T::MaxDomainKeyLen>,
            payload_hash: &[u8; 32],
            nonce: u64,
        ) -> Vec<u8> {
            (
                b"ORACLELINK:SUBMIT:v1",
                oracle_id,
                source_chain_id,
                domain_key.as_slice(),
                payload_hash,
                nonce,
            )
                .encode()
        }

        fn verify_threshold(
            oracle_id: OracleId,
            message: &[u8],
            signatures: &Vec<(sp_runtime::MultiSigner, MultiSignature)>,
        ) -> DispatchResult {
            ensure!(
                signatures.len() as u32 <= T::MaxSignatures::get(),
                Error::<T>::TooManySignatures
            );
            let cfg =
                registry::Pallet::<T>::get_oracle(oracle_id).ok_or(Error::<T>::OracleError)?;
            let authorized = registry::Pallet::<T>::get_signers(oracle_id);
            ensure!(!authorized.is_empty(), Error::<T>::OracleError);

            let mut valid: u32 = 0;
            for (i, (signer, sig)) in signatures.iter().enumerate() {
                for (signer2, _) in signatures.iter().skip(i + 1) {
                    ensure!(signer != signer2, Error::<T>::DuplicateSignerInProof);
                }
                if !authorized.iter().any(|a| a == signer) {
                    return Err(Error::<T>::SignerNotAuthorized.into());
                }
                if sig.verify(message, &signer.clone().into_account()) {
                    valid = valid.saturating_add(1);
                } else {
                    return Err(Error::<T>::BadSignature.into());
                }
            }
            ensure!(valid >= cfg.threshold, Error::<T>::ThresholdNotMet);
            Ok(())
        }

        fn historical_check(
            oracle_id: OracleId,
            domain_key: &BoundedVec<u8, T::MaxDomainKeyLen>,
            payload_hash: [u8; 32],
        ) -> (VerificationOutcome, Option<EntryId>) {
            if let Some(last_id) = LastSeenByOracleDomain::<T>::get((oracle_id, domain_key.clone()))
            {
                if let Some(prev) = LedgerEntries::<T>::get(last_id) {
                    if prev.payload_hash == payload_hash {
                        return (VerificationOutcome::ConsistentWithHistory, None);
                    } else {
                        return (VerificationOutcome::ConflictDetected, Some(last_id));
                    }
                }
                (VerificationOutcome::HistoryCheckFailed, None)
            } else {
                (VerificationOutcome::New, None)
            }
        }

        fn entry_hash(
            entry_id: EntryId,
            oracle_id: OracleId,
            domain_key: &BoundedVec<u8, T::MaxDomainKeyLen>,
            payload_hash: &[u8; 32],
            prev_root: &[u8; 32],
        ) -> [u8; 32] {
            blake2_256(
                &(
                    entry_id,
                    oracle_id,
                    domain_key.as_slice(),
                    payload_hash,
                    prev_root,
                )
                    .encode(),
            )
        }

        fn accumulator_root(prev_root: &[u8; 32], entry_hash: &[u8; 32]) -> [u8; 32] {
            blake2_256(&(prev_root, entry_hash).encode())
        }
    }
}
