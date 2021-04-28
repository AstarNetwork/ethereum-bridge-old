#![cfg_attr(not(feature = "std"), no_std)]
#![recursion_limit = "128"]

use sp_std::prelude::*;
use sp_std::vec::Vec;
use codec::{Encode, Decode};
use frame_support::{traits::Get, dispatch::DispatchResult,
     ensure, decl_error, decl_event, decl_module, decl_storage};
use frame_system::{ensure_signed, ensure_root};
use sp_runtime::RuntimeDebug;
use eth_primitives::{
    header::EthereumHeader,
    pow::{EthashPartial, EthashSeal},
    network_type::EthereumNetworkType,
    EthereumBlockNumber, H256, U256,
    EthereumReceipt, EthereumReceiptProof
};
use eth_primitives::keccak;

#[derive(Default, Encode, Decode, RuntimeDebug, Clone)]
pub struct HeaderChainLatest {
    pub hash: H256,
    pub number: EthereumBlockNumber,
    pub parent_hash: H256,
    pub total_difficulty: U256,
}


pub trait Config: frame_system::Config {
    /// light client event
    type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;

    /// Ethereum network type, currently, the options are ropsten/mainnet
    type EthereumNetwork: Get<EthereumNetworkType>;

    /// Confirmation buffer
    /// After `Confirmations` number, we can see the header a final one.
    type Confirmations: Get<EthereumBlockNumber>;

}


decl_error! {
	pub enum Error for Module<T: Config> {
	    /// header.hash does not match the calculated one
        HeaderHashMismatch,
        /// Genesis header does not exist
        GenesisHeaderNE,
        /// Too early
        TooEarly,
        /// Too late
        TooLate,
        /// prev header does not exist
        PrevHeaderNE,
        /// verify_block_basic verification failed
        BlockBasicVF,
        /// Difficulty verification failed
        DifficultyVF,
        /// Mix hash verification failed
        MixHashVF,
        /// Mix hash calculation failed
        MixHashCF,
        /// Fail to parse seal from the header
        SealParseErr,
        /// Fail to set the genesis header after it is already set
        GenesisSetFailed,
        /// Fail to update the best number
        AuthBestNumberUF,
        /// Header does not exist
        HeaderNE,
        /// No Finalized header
        FinalizedHeaderNE,
        /// receipt verification fail
        ReceiptVF,

	}
}

decl_event! {
    pub enum Event<T> where <T as frame_system::Config>::AccountId {
        SetGenesisHeader(EthereumHeader),
        UpdateBestNumber(EthereumBlockNumber),
        Maintain(EthereumHeader, AccountId),
    }
}

decl_storage! {
    trait Store for Module<T: Config> as EthLight {
        /// Set the beginning of the chain
        pub GenesisHeader get(fn genesis_header): Option<EthereumHeader>;

        /// Hash of best block header submitted by root/authority
        /// TODO: remove this to adapt the header chain re-org
        pub AuthBestNumber get(fn auth_best_number): EthereumBlockNumber;

        /// Detailed info of a header(Hash)
        pub Header get(fn header): map hasher(twox_64_concat) EthereumBlockNumber => Option<EthereumHeader>;

        /// header chain
        pub HeaderChain get(fn header_chain): Option<HeaderChainLatest>;


    }
}

decl_module! {
    pub struct Module<T: Config> for enum Call where origin: T::Origin {
        fn deposit_event() = default;

        type Error = Error<T>;

        #[weight = 0]
        fn maintain_header_chain(origin, header: EthereumHeader) -> DispatchResult {
            // TODO: add permission check later
            let who = ensure_signed(origin)?;
            // get current network
            let ethash_params = match T::EthereumNetwork::get() {
			    EthereumNetworkType::Mainnet => EthashPartial::production(),
			    EthereumNetworkType::Ropsten => EthashPartial::ropsten_testnet(),
		    };

            // verify the header
            {
                Self::verify_header_basic(&header)?;
                Self::check_difficulty(&header, &ethash_params)?;
                Self::check_pow(&header, &ethash_params)?;
            }
            // add the latest header
            Self::append_header(&header);

            Self::deposit_event(RawEvent::Maintain(header, who));

            Ok(())
        }

        #[weight = 0]
        fn set_genesis_header(origin, header: EthereumHeader) {
            ensure_root(origin)?;
            ensure!(Self::genesis_header().is_none(), Error::<T>::GenesisSetFailed);
            GenesisHeader::put(&header);
            Self::deposit_event(RawEvent::SetGenesisHeader(header));
        }

        #[weight = 0]
        fn verify_receipt(
            origin, 
            eth_block_number: EthereumBlockNumber,
            proof: EthereumReceiptProof,
            receipt: EthereumReceipt
        ) -> DispatchResult {
            // TODO: maintain a set of authorities
            let who = ensure_signed(origin)?;
            // make sure the eth block is finalized
            let latest_finalized_header = Self::header_chain().ok_or(Error::<T>::FinalizedHeaderNE)?;
            let finalized_block_number = latest_finalized_header.number;
            ensure!(eth_block_number <= finalized_block_number, Error::<T>::TooEarly);
            // get block header
            let block_header = Self::header(&eth_block_number).ok_or(Error::<T>::HeaderNE)?;

            let expected_receipt = EthereumReceipt::verify_proof_and_generate(
                &block_header.receipts_root,
                &proof
            ).map_err(|_| Error::<T>::ReceiptVF)?;
            // ensure we get the right receipt
            ensure!(expected_receipt == receipt, Error::<T>::ReceiptVF);

            // TODO: do something 
            Ok(())
        }
    }
}

impl<T: Config> Module<T> {

    /// Verify basic block params
    fn verify_header_basic(header: &EthereumHeader) -> DispatchResult {
        ensure!(
			header.hash() == header.re_compute_hash(),
			<Error<T>>::HeaderHashMismatch
		);
        log::trace!(
            target: "verify_header_basic",
            "Hash {:?} is OK", header.hash()
        );
        let genesis_header = Self::genesis_header()
            .ok_or(())
            .map_err(|_| Error::<T>::GenesisHeaderNE)?
            .number;
        ensure!(header.number >= genesis_header, Error::<T>::TooEarly);
        log::trace!(target: "verify_header_basic", "Head Number OK");

        // make sure the prev header exist
        let prev_header = Self::header(header.number - 1).ok_or(<Error<T>>::PrevHeaderNE)?;

        ensure!(
			header.parent_hash == prev_header.hash(),
			<Error<T>>::HeaderHashMismatch
		);

        Ok(())
    }

    fn check_difficulty(
        header: &EthereumHeader,
        ethash_params: &EthashPartial
    ) -> DispatchResult {
        ethash_params
            .verify_block_basic(header)
            .map_err(|_| <Error<T>>::BlockBasicVF)?;
        log::trace!(target: "verify_block_basic", "PASS");
        let prev_header = Self::header(header.number - 1).ok_or(<Error<T>>::HeaderNE)?;
        let difficulty = ethash_params.calculate_difficulty(header, &prev_header);
        ensure!(difficulty == *header.difficulty(), <Error<T>>::DifficultyVF);
        log::trace!(target: "check_difficulty", "PASS");

        Ok(())
    }

    fn check_pow(header: &EthereumHeader, ethash_params: &EthashPartial) -> DispatchResult {
        let seal = EthashSeal::parse_seal(header.seal())
            .map_err(|_| Error::<T>::SealParseErr)?;
        let calculated_mix_hash = ethash_params.calculate_mixhash(header).map_err(|e| Error::<T>::MixHashCF)?;
        let mix_hash = seal.mix_hash;
        ensure!(mix_hash == calculated_mix_hash, Error::<T>::MixHashVF);
        log::trace!(target: "check_pow", "Mixhash OK");
        Ok(())
    }

    fn append_header(header: &EthereumHeader) -> DispatchResult {
        log::trace!(target: "Add a new header", "{:?}", header);
        let number= header.number();
        let confirmation_num = T::Confirmations::get();
        let best_number = Self::auth_best_number();
        // Make sure the we only put the final header, due to the temporary lack of reorg
        ensure!(
            // TODO: these two errors are ambiguous
            number < best_number
                .checked_sub(confirmation_num)
                .ok_or(())
                .map_err(|_| Error::<T>::TooEarly)?, Error::<T>::TooEarly
        );
        // add header
        Header::insert(&number, header);
        HeaderChain::try_mutate(|header_chain| -> DispatchResult {
            let mut headers = header_chain.take().unwrap_or_default();
            headers.parent_hash = *header.parent_hash();
            headers.hash = header.hash();
            headers.number = header.number();
            headers.total_difficulty += *header.difficulty();
            *header_chain = Some(headers);
            Ok(())
        })
    }
}