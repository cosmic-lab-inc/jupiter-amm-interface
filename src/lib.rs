use anyhow::{anyhow, Context, Error, Result};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use solana_account_decoder::{UiAccount, UiAccountEncoding};
use std::collections::HashSet;
use std::{collections::HashMap, convert::TryFrom, str::FromStr};
mod swap;
pub use swap::{Side, Swap};

/// An abstraction in order to share reserve mints and necessary data
use solana_sdk::{account::Account, instruction::AccountMeta, pubkey::Pubkey};

#[derive(Serialize, Deserialize, PartialEq, Clone, Copy, Default, Debug)]
pub enum SwapMode {
    #[default]
    ExactIn,
    ExactOut,
}

impl FromStr for SwapMode {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "ExactIn" => Ok(SwapMode::ExactIn),
            "ExactOut" => Ok(SwapMode::ExactOut),
            _ => Err(anyhow!("{} is not a valid SwapMode", s)),
        }
    }
}

#[derive(Debug)]
pub struct QuoteParams {
    pub amount: u64,
    pub input_mint: Pubkey,
    pub output_mint: Pubkey,
    pub swap_mode: SwapMode,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Quote {
    pub not_enough_liquidity: bool,
    pub min_in_amount: Option<u64>,
    pub min_out_amount: Option<u64>,
    pub in_amount: u64,
    pub out_amount: u64,
    pub fee_amount: u64,
    pub fee_mint: Pubkey,
    pub fee_pct: Decimal,
}

pub type QuoteMintToReferrer = HashMap<Pubkey, Pubkey>;

pub struct SwapParams<'a, 'b> {
    pub in_amount: u64,
    pub out_amount: u64,
    pub source_mint: Pubkey,
    pub destination_mint: Pubkey,
    pub source_token_account: Pubkey,
    pub destination_token_account: Pubkey,
    /// This can be the user or the program authority over the source_token_account.
    pub token_transfer_authority: Pubkey,
    pub open_order_address: Option<Pubkey>,
    pub quote_mint_to_referrer: Option<&'a QuoteMintToReferrer>,
    pub jupiter_program_id: &'b Pubkey,
    /// Instead of returning the relevant Err, replace dynamic accounts with the default Pubkey
    /// This is useful for crawling market with no tick array
    pub missing_dynamic_accounts_as_default: bool,
}

impl<'a, 'b> SwapParams<'a, 'b> {
    /// A placeholder to indicate an optional account or used as a terminator when consuming remaining accounts
    /// Using the jupiter program id
    pub fn placeholder_account_meta(&self) -> AccountMeta {
        AccountMeta::new_readonly(*self.jupiter_program_id, false)
    }
}

pub struct SwapAndAccountMetas {
    pub swap: Swap,
    pub account_metas: Vec<AccountMeta>,
}

/// Amm might trigger a setup step for the user
#[derive(Clone)]
pub enum AmmUserSetup {
    SerumDexOpenOrdersSetup { market: Pubkey, program_id: Pubkey },
}

pub type AccountMap = HashMap<Pubkey, Account>;

pub fn try_get_account_data<'a>(account_map: &'a AccountMap, address: &Pubkey) -> Result<&'a [u8]> {
    account_map
        .get(address)
        .map(|account| account.data.as_slice())
        .with_context(|| format!("Could not find address: {address}"))
}

pub trait Amm {
    // Maybe trait was made too restrictive?
    fn from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self>
    where
        Self: Sized;
    /// A human readable label of the underlying DEX
    fn label(&self) -> String;
    fn program_id(&self) -> Pubkey;
    /// The pool state or market state address
    fn key(&self) -> Pubkey;
    /// The mints that can be traded
    fn get_reserve_mints(&self) -> Vec<Pubkey>;
    /// The accounts necessary to produce a quote
    fn get_accounts_to_update(&self) -> Vec<Pubkey>;
    /// Picks necessary accounts to update it's internal state
    /// Heavy deserialization and precomputation caching should be done in this function
    fn update(&mut self, account_map: &AccountMap) -> Result<()>;

    fn quote(&self, quote_params: &QuoteParams) -> Result<Quote>;

    /// Indicates which Swap has to be performed along with all the necessary account metas
    fn get_swap_and_account_metas(&self, swap_params: &SwapParams) -> Result<SwapAndAccountMetas>;

    /// Indicates if get_accounts_to_update might return a non constant vec
    fn has_dynamic_accounts(&self) -> bool {
        false
    }

    /// Indicates whether `update` needs to be called before `get_reserve_mints`
    fn requires_update_for_reserve_mints(&self) -> bool {
        false
    }

    // Indicates that whether ExactOut mode is supported
    fn supports_exact_out(&self) -> bool {
        false
    }

    fn get_user_setup(&self) -> Option<AmmUserSetup> {
        None
    }

    fn clone_amm(&self) -> Box<dyn Amm + Send + Sync>;

    /// It can only trade in one direction from its first mint to second mint, assuming it is a two mint AMM
    fn unidirectional(&self) -> bool {
        false
    }

    /// For testing purposes, provide a mapping of dependency programs to function
    fn program_dependencies(&self) -> Vec<(Pubkey, String)> {
        vec![]
    }

    fn get_accounts_len(&self) -> usize {
        32 // Default to a near whole legacy transaction to penalize no implementation
    }

    /// The identifier of the underlying liquidity
    ///
    /// Example:
    /// For RaydiumAmm uses Openbook market A this will return Some(A)
    /// For Openbook market A, it will also return Some(A)
    fn underlying_liquidities(&self) -> Option<HashSet<Pubkey>> {
        None
    }

    /// Provides a shortcut to establish if the AMM can be used for trading
    /// If the market is active at all
    fn is_active(&self) -> bool {
        true
    }
}

impl Clone for Box<dyn Amm + Send + Sync> {
    fn clone(&self) -> Box<dyn Amm + Send + Sync> {
        self.clone_amm()
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct KeyedAccount {
    pub key: Pubkey,
    pub account: Account,
    pub params: Option<Value>,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq)]
pub struct KeyedUiAccount {
    pub pubkey: String,
    #[serde(flatten)]
    pub ui_account: UiAccount,
    /// Additional data an Amm requires, Amm dependent and decoded in the Amm implementation
    pub params: Option<Value>,
}

impl From<KeyedAccount> for KeyedUiAccount {
    fn from(keyed_account: KeyedAccount) -> Self {
        let KeyedAccount {
            key,
            account,
            params,
        } = keyed_account;
        let ui_account = UiAccount::encode(&key, &account, UiAccountEncoding::Base64, None, None);

        KeyedUiAccount {
            pubkey: key.to_string(),
            ui_account,
            params,
        }
    }
}

impl TryFrom<KeyedUiAccount> for KeyedAccount {
    type Error = Error;

    fn try_from(keyed_ui_account: KeyedUiAccount) -> Result<Self, Self::Error> {
        let KeyedUiAccount {
            pubkey,
            ui_account,
            params,
        } = keyed_ui_account;
        let account = ui_account
            .decode()
            .unwrap_or_else(|| panic!("Failed to decode ui_account for {}", pubkey));

        Ok(KeyedAccount {
            key: Pubkey::from_str(&pubkey)?,
            account,
            params,
        })
    }
}