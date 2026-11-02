use serde::{Deserialize, Serialize};
use std::fmt;

/// Currencies that are conventionally quoted with zero decimal places.
/// Mirrors the list Stripe uses for the same purpose - these currencies'
/// smallest unit already represents a whole, spendable amount.
const ZERO_DECIMAL_CURRENCIES: &[&str] = &[
    "BIF", "CLP", "DJF", "GNF", "IDR", "JPY", "KMF", "KRW", "MGA", "PYG", "RWF", "UGX", "VND",
    "VUV", "XAF", "XOF", "XPF",
];

fn decimals_for_currency(code: &str) -> u32 {
    if ZERO_DECIMAL_CURRENCIES.contains(&code.to_uppercase().as_str()) {
        0
    } else {
        2
    }
}

/// A monetary amount stored as integer minor units to avoid float rounding
/// error and sqlite decimal-storage friction. For a 2-decimal currency like
/// USD, `minor_units: 150` means $1.50. For a 0-decimal currency like IDR,
/// `minor_units: 15000` means Rp 15.000.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Money {
    pub minor_units: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyAmount {
    pub amount: Money,
    pub currency: String,
}

impl Money {
    pub fn from_minor_units(minor_units: i64) -> Self {
        Self { minor_units }
    }

    /// Build from a human-entered decimal amount (e.g. 500.0) for the given
    /// currency, respecting that currency's conventional decimal places.
    pub fn from_major(amount: f64, currency: &str) -> Self {
        let decimals = decimals_for_currency(currency);
        let scale = 10i64.pow(decimals);
        Self {
            minor_units: (amount * scale as f64).round() as i64,
        }
    }

    pub fn checked_add(self, other: Money) -> Self {
        Self {
            minor_units: self.minor_units + other.minor_units,
        }
    }

    pub fn checked_sub(self, other: Money) -> Self {
        Self {
            minor_units: self.minor_units - other.minor_units,
        }
    }

    pub fn zero() -> Self {
        Self { minor_units: 0 }
    }

    /// Format for display in the given currency, e.g. `Money::from_minor_units(150).format_in("USD")` -> "1.50".
    pub fn format_in(&self, currency: &str) -> String {
        let decimals = decimals_for_currency(currency);
        if decimals == 0 {
            return self.minor_units.to_string();
        }
        let scale = 10i64.pow(decimals);
        let whole = self.minor_units / scale;
        let frac = (self.minor_units % scale).abs();
        format!("{whole}.{frac:0width$}", width = decimals as usize)
    }
}

impl CurrencyAmount {
    pub fn new(major_amount: f64, currency: impl Into<String>) -> Self {
        let currency = currency.into();
        Self {
            amount: Money::from_major(major_amount, &currency),
            currency,
        }
    }
}

impl fmt::Display for CurrencyAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}",
            self.amount.format_in(&self.currency),
            self.currency
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_decimal_currency_round_trips() {
        let amt = CurrencyAmount::new(12.5, "USD");
        assert_eq!(amt.amount.minor_units, 1250);
        assert_eq!(amt.amount.format_in("USD"), "12.50");
    }

    #[test]
    fn zero_decimal_currency_has_no_fraction() {
        let amt = CurrencyAmount::new(15000.0, "IDR");
        assert_eq!(amt.amount.minor_units, 15000);
        assert_eq!(amt.amount.format_in("IDR"), "15000");
    }

    #[test]
    fn add_and_sub_are_exact() {
        let a = Money::from_major(10.10, "USD");
        let b = Money::from_major(0.05, "USD");
        assert_eq!(a.checked_add(b).minor_units, 1015);
        assert_eq!(a.checked_sub(b).minor_units, 1005);
    }
}
