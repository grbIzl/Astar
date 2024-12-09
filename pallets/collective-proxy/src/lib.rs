// This file is part of Astar.

// Copyright (C) Stake Technologies Pte.Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later

// Astar is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Astar is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Astar. If not, see <http://www.gnu.org/licenses/>.

#![cfg_attr(not(feature = "std"), no_std)]

use frame_support::{
    dispatch::GetDispatchInfo,
    pallet_prelude::*,
    traits::{InstanceFilter, IsType, OriginTrait},
};
use frame_system::pallet_prelude::*;
use sp_runtime::traits::Dispatchable;
use sp_std::prelude::*;

pub use pallet::*;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;

pub mod weights;
pub use weights::WeightInfo;

/// The parameters under which a particular account has a proxy relationship with some other
/// account.
#[derive(
    Encode,
    Decode,
    Clone,
    Copy,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    MaxEncodedLen,
    TypeInfo,
)]
pub struct ProxyDefinition<AccountId, CallFilter> {
    /// The account which may act on behalf of another.
    pub proxy: AccountId,
    /// A value defining the subset of calls that it is allowed to make.
    pub filter: CallFilter,
}

#[frame_support::pallet]
pub mod pallet {
    use super::*;

    /// The current storage version.
    pub const STORAGE_VERSION: StorageVersion = StorageVersion::new(1);

    #[pallet::pallet]
    #[pallet::storage_version(STORAGE_VERSION)]
    pub struct Pallet<T>(_);

    // TODO: The pallet is intentionally very basic. It could be improved to handle more origins, more aliases, etc.
    // There could also be different instances, if such approach was needed.
    // However, it's supposed to be the simplest solution possible to cover a specific scenario.
    // Pallet is stateless and can easily be upgraded in the future.

    /// Configuration trait.
    #[pallet::config]
    pub trait Config: frame_system::Config {
        /// The overarching event type.
        type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

        /// The overarching call type.
        type RuntimeCall: Parameter
            + Dispatchable<RuntimeOrigin = Self::RuntimeOrigin>
            + GetDispatchInfo
            + From<frame_system::Call<Self>>
            + IsType<<Self as frame_system::Config>::RuntimeCall>;

        /// Origin that can act on behalf of the collective.
        type CollectiveProxy: EnsureOrigin<<Self as frame_system::Config>::RuntimeOrigin>;

        /// Origin with permissions to add and remove proxies for the collective.
        type ProxyAdmin: EnsureOrigin<<Self as frame_system::Config>::RuntimeOrigin>;

        /// Filter to determine whether a call can be executed or not.
        type CallFilter: InstanceFilter<<Self as Config>::RuntimeCall>
            + Member
            + Clone
            + Encode
            + Decode
            + MaxEncodedLen
            + TypeInfo
            + Default;

        /// The maximum amount of proxies allowed for a single account.
        #[pallet::constant]
        type MaxProxies: Get<u32>;

        /// Weight info
        type WeightInfo: WeightInfo;
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        /// Community proxy call executed successfully.
        CollectiveProxyExecuted { result: DispatchResult },
    }

    #[pallet::error]
    pub enum Error<T> {
        /// There are too many proxies registered
        TooManyProxies,
        /// Proxy registration not found.
        NotFound,
    }

    /// The set of account proxies
    #[pallet::storage]
    pub type Proxies<T: Config> = StorageValue<
        _,
        BoundedVec<
            ProxyDefinition<T::AccountId, T::CallFilter>,
            T::MaxProxies,
        >,
        ValueQuery,
    >;

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Executes the call on a behalf of an aliased account.
        ///
        /// The `origin` of the call is supposed to be a _collective_ (but can be anything) which can dispatch `call` on behalf of the aliased account.
        /// It's essentially a proxy call that can be made by arbitrary origin type.
        #[pallet::call_index(0)]
        #[pallet::weight({
			let di = call.get_dispatch_info();
			(T::WeightInfo::execute_call().saturating_add(di.weight), di.class)
		})]
        pub fn execute_call(
            origin: OriginFor<T>,
            proxy: T::AccountId,
            call: Box<<T as Config>::RuntimeCall>,
        ) -> DispatchResult {
            // Ensure origin is valid.
            T::CollectiveProxy::ensure_origin(origin)?;

            let def = Self::find_proxy(proxy)?;

            // Account authentication is ensured by the `CollectiveProxy` origin check.
            let mut origin: T::RuntimeOrigin =
                frame_system::RawOrigin::Signed(def.proxy).into();

            // Ensure custom filter is applied.
            origin.add_filter(move |c: &<T as frame_system::Config>::RuntimeCall| {
                let c = <T as Config>::RuntimeCall::from_ref(c);
                def.filter.filter(c)
            });

            // Dispatch the call.
            let e = call.dispatch(origin);
            Self::deposit_event(Event::CollectiveProxyExecuted {
                result: e.map(|_| ()).map_err(|e| e.error),
            });

            Ok(())
        }

        /// Register a proxy account for the sender that is able to make calls on its behalf.
        ///
        /// The dispatch origin for this call must be _Signed_.
        ///
        /// Parameters:
        /// - `proxy`: The account that the `caller` would like to make a proxy.
        /// - `filter`: Call filter used for the proxy
        #[pallet::call_index(1)]
        #[pallet::weight(T::WeightInfo::add_proxy(T::MaxProxies::get()))]
        pub fn add_proxy(
            origin: OriginFor<T>,
            proxy: T::AccountId,
            filter: T::CallFilter,
        ) -> DispatchResult {
            T::ProxyAdmin::ensure_origin(origin)?;
            Proxies::<T>::try_mutate(|proxies| -> Result<(), DispatchError> {
                if !proxies.iter().any(|p| p.proxy == proxy && p.filter.is_superset(&filter)) {
                    let proxy_def = ProxyDefinition {
                        proxy: proxy.clone(),
                        filter: filter.clone(),
                    };
                    proxies.try_push(proxy_def).map_err(|_| Error::<T>::TooManyProxies)?;
                }
                Ok(())
            })
        }

        /// Unregister a proxy account for the sender.
        ///
        /// The dispatch origin for this call must be _Signed_.
        ///
        /// Parameters:
        /// - `proxy`: The account that the `caller` would like to remove as a proxy.
        /// - `filter`: Call filter used for the proxy
        #[pallet::call_index(2)]
        #[pallet::weight(T::WeightInfo::remove_proxy(T::MaxProxies::get()))]
        pub fn remove_proxy(
            origin: OriginFor<T>,
            proxy: T::AccountId,
            filter: T::CallFilter,
        ) -> DispatchResult {
            T::ProxyAdmin::ensure_origin(origin)?;
            Proxies::<T>::try_mutate(|proxies| -> Result<(), DispatchError> {
                let proxy_def = ProxyDefinition {
                    proxy: proxy.clone(),
                    filter: filter.clone(),
                };
                proxies.retain(|def| def != &proxy_def);
                Ok(())
            })
        }
    }

    impl<T: Config> Pallet<T> {
        pub fn find_proxy(
            proxy: T::AccountId,
        ) -> Result<ProxyDefinition<T::AccountId, T::CallFilter>, DispatchError> {
            let f = |x: &ProxyDefinition<T::AccountId, T::CallFilter>| -> bool {
                x.proxy == proxy
            };
            Ok(Proxies::<T>::get().into_iter().find(f).ok_or(Error::<T>::NotFound)?)
        }
    }
}
