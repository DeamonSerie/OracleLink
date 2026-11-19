#![cfg_attr(not(feature = "std"), no_std)]

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use frame_support::pallet_prelude::*;
    use frame_system::pallet_prelude::*;
    use sp_runtime::traits::{AtLeast32BitUnsigned, CheckedAdd, CheckedSub, Zero};
    use sp_runtime::ArithmeticError;

    #[pallet::config]
    pub trait Config: frame_system::Config {
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;
        type Balance: Member
            + Parameter
            + MaxEncodedLen
            + Copy
            + Default
            + CheckedAdd
            + CheckedSub
            + Zero
            + AtLeast32BitUnsigned
            + TypeInfo;
        #[pallet::constant]
        type ExistentialDeposit: Get<Self::Balance>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn total_issuance)]
    pub type TotalIssuance<T: Config> = StorageValue<_, T::Balance, ValueQuery>;

    #[pallet::storage]
    #[pallet::getter(fn balance_of)]
    pub type Balances<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, T::Balance, ValueQuery>;

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        Transfer {
            from: T::AccountId,
            to: T::AccountId,
            amount: T::Balance,
        },
        Endowed {
            account: T::AccountId,
            amount: T::Balance,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        InsufficientBalance,
        ExistentialDeposit,
        Overflow,
    }

    #[pallet::genesis_config]
    pub struct GenesisConfig<T: Config> {
        pub balances: Vec<(T::AccountId, T::Balance)>,
    }

    impl<T: Config> Default for GenesisConfig<T> {
        fn default() -> Self {
            Self { balances: vec![] }
        }
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            let mut total = T::Balance::zero();
            for (who, amount) in &self.balances {
                <Balances<T>>::insert(who, *amount);
                total = total.checked_add(amount).expect("genesis overflow");
            }
            <TotalIssuance<T>>::put(total);
        }
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Transfer OCC (main currency). Fees are paid in TEN via transaction payment.
        #[pallet::call_index(0)]
        #[pallet::weight(Weight::from_parts(10_000, 0) + T::DbWeight::get().reads_writes(2, 2))]
        pub fn transfer(
            origin: OriginFor<T>,
            dest: T::AccountId,
            #[pallet::compact] value: T::Balance,
        ) -> DispatchResult {
            let from = ensure_signed(origin)?;
            Self::do_transfer(&from, &dest, value)?;
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        pub fn do_transfer(
            from: &T::AccountId,
            to: &T::AccountId,
            value: T::Balance,
        ) -> DispatchResult {
            if value.is_zero() || from == to {
                return Ok(());
            }
            <Balances<T>>::try_mutate(from, |b| -> DispatchResult {
                let new_from = b
                    .checked_sub(&value)
                    .ok_or(Error::<T>::InsufficientBalance)?;
                ensure!(
                    new_from.is_zero() || new_from >= T::ExistentialDeposit::get(),
                    Error::<T>::ExistentialDeposit
                );
                *b = new_from;
                Ok(())
            })?;
            <Balances<T>>::try_mutate(to, |b| -> DispatchResult {
                let new_to = b.checked_add(&value).ok_or(ArithmeticError::Overflow)?;
                ensure!(
                    new_to >= T::ExistentialDeposit::get(),
                    Error::<T>::ExistentialDeposit
                );
                *b = new_to;
                Ok(())
            })?;
            Self::deposit_event(Event::Transfer {
                from: from.clone(),
                to: to.clone(),
                amount: value,
            });
            Ok(())
        }

        pub fn mint(to: &T::AccountId, value: T::Balance) -> DispatchResult {
            <Balances<T>>::try_mutate(to, |b| -> DispatchResult {
                let new_to = b.checked_add(&value).ok_or(ArithmeticError::Overflow)?;
                *b = new_to;
                Ok(())
            })?;
            <TotalIssuance<T>>::try_mutate(|t| -> DispatchResult {
                *t = t.checked_add(&value).ok_or(ArithmeticError::Overflow)?;
                Ok(())
            })?;
            Ok(())
        }
    }
}
