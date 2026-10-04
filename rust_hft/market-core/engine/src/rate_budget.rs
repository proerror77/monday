//! In-process admission budgets owned by the single execution worker.
//!
//! Each operation consumes one request from both its account and egress dimensions.
//! Missing dimensions deny admission. Rejected requests consume neither dimension;
//! admitted attempts remain charged even if the adapter subsequently rejects them.

use hft_core::{AccountId, ProductType, VenueId};
use std::collections::HashMap;
use tokio::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateBudgetOperation {
    NewOrder,
    Cancel,
    CancelAll,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RateBudgetScope {
    Account(AccountId),
    /// Runtime-bound outbound identity, never supplied by an order or strategy.
    Egress(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RateBudgetKey {
    pub venue: VenueId,
    pub product: ProductType,
    pub operation: RateBudgetOperation,
    pub scope: RateBudgetScope,
}

#[derive(Debug, Clone)]
pub struct RateBudgetSpec {
    pub key: RateBudgetKey,
    pub max_requests: u32,
    /// Full quota refills after this monotonic interval. Late observation starts
    /// the next interval at that observation, conservatively avoiding catch-up bursts.
    pub refill_interval: Duration,
}

#[derive(Debug)]
struct BudgetBucket {
    spec: RateBudgetSpec,
    remaining: u32,
    last_refill: Instant,
}

impl BudgetBucket {
    fn refill(&mut self, now: Instant) {
        if now.saturating_duration_since(self.last_refill) >= self.spec.refill_interval {
            self.remaining = self.spec.max_requests;
            self.last_refill = now;
        }
    }
}

/// Empty is deny-all. State is local to one worker and is never cloned or refunded.
#[derive(Debug, Default)]
pub struct RateBudget {
    buckets: HashMap<RateBudgetKey, BudgetBucket>,
}

impl RateBudget {
    pub fn new(specs: impl IntoIterator<Item = RateBudgetSpec>) -> Result<Self, String> {
        let mut budget = Self::default();
        for spec in specs {
            budget.add(spec)?;
        }
        Ok(budget)
    }

    pub(crate) fn add(&mut self, spec: RateBudgetSpec) -> Result<(), String> {
        let identity = match &spec.key.scope {
            RateBudgetScope::Account(account) => &account.0,
            RateBudgetScope::Egress(egress) => egress,
        };
        if identity.trim().is_empty() {
            return Err("rate budget identity is empty".to_string());
        }
        if spec.max_requests == 0 || spec.refill_interval.is_zero() {
            return Err("rate budget quota and refill interval must be positive".to_string());
        }
        if self.buckets.contains_key(&spec.key) {
            return Err("duplicate rate budget dimension".to_string());
        }
        self.buckets.insert(
            spec.key.clone(),
            BudgetBucket {
                remaining: spec.max_requests,
                last_refill: Instant::now(),
                spec,
            },
        );
        Ok(())
    }

    pub fn admit(
        &mut self,
        venue: VenueId,
        product: ProductType,
        operation: RateBudgetOperation,
        account: &AccountId,
        egress: &str,
    ) -> Result<(), String> {
        self.admit_at(venue, product, operation, account, egress, Instant::now())
    }

    fn admit_at(
        &mut self,
        venue: VenueId,
        product: ProductType,
        operation: RateBudgetOperation,
        account: &AccountId,
        egress: &str,
        now: Instant,
    ) -> Result<(), String> {
        let keys = [
            RateBudgetKey {
                venue,
                product,
                operation,
                scope: RateBudgetScope::Account(account.clone()),
            },
            RateBudgetKey {
                venue,
                product,
                operation,
                scope: RateBudgetScope::Egress(egress.to_string()),
            },
        ];
        // Validate every dimension before decrementing any of them. No await or
        // second writer can interleave the check and charge in this worker.
        for key in &keys {
            let dimension = match &key.scope {
                RateBudgetScope::Account(_) => "account",
                RateBudgetScope::Egress(_) => "egress",
            };
            let bucket = self
                .buckets
                .get_mut(key)
                .ok_or_else(|| format!("unconfigured {dimension} rate budget for {operation:?}"))?;
            bucket.refill(now);
            if bucket.remaining == 0 {
                return Err(format!(
                    "{dimension} rate budget exhausted for {operation:?}"
                ));
            }
        }
        for key in &keys {
            self.buckets
                .get_mut(key)
                .expect("validated dimension")
                .remaining -= 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(scope: RateBudgetScope, operation: RateBudgetOperation, quota: u32) -> RateBudgetSpec {
        RateBudgetSpec {
            key: RateBudgetKey {
                venue: VenueId::BYBIT,
                product: ProductType::Spot,
                operation,
                scope,
            },
            max_requests: quota,
            refill_interval: Duration::from_secs(10),
        }
    }

    fn pair(
        account: &str,
        egress: &str,
        operation: RateBudgetOperation,
        quota: u32,
    ) -> Vec<RateBudgetSpec> {
        vec![
            spec(
                RateBudgetScope::Account(AccountId(account.into())),
                operation,
                quota,
            ),
            spec(RateBudgetScope::Egress(egress.into()), operation, quota),
        ]
    }

    fn admit(
        budget: &mut RateBudget,
        account: &str,
        egress: &str,
        operation: RateBudgetOperation,
        now: Instant,
    ) -> Result<(), String> {
        budget.admit_at(
            VenueId::BYBIT,
            ProductType::Spot,
            operation,
            &AccountId(account.into()),
            egress,
            now,
        )
    }

    #[test]
    fn rate_budget_exhaustion_rejects_until_refill() {
        let mut budget =
            RateBudget::new(pair("a", "ip-a", RateBudgetOperation::NewOrder, 1)).unwrap();
        let now = Instant::now();
        assert!(admit(&mut budget, "a", "ip-a", RateBudgetOperation::NewOrder, now).is_ok());
        assert_eq!(
            admit(&mut budget, "a", "ip-a", RateBudgetOperation::NewOrder, now).unwrap_err(),
            "account rate budget exhausted for NewOrder"
        );
        assert!(admit(
            &mut budget,
            "a",
            "ip-a",
            RateBudgetOperation::NewOrder,
            now + Duration::from_secs(9)
        )
        .is_err());
        assert!(admit(
            &mut budget,
            "a",
            "ip-a",
            RateBudgetOperation::NewOrder,
            now + Duration::from_secs(10)
        )
        .is_ok());
    }

    #[test]
    fn rate_budget_operations_and_identities_are_independent() {
        let operations = [
            RateBudgetOperation::NewOrder,
            RateBudgetOperation::Cancel,
            RateBudgetOperation::CancelAll,
        ];
        let specs = operations
            .into_iter()
            .flat_map(|op| pair("a", "ip-a", op, 1))
            .chain(pair("b", "ip-b", RateBudgetOperation::NewOrder, 1));
        let mut budget = RateBudget::new(specs).unwrap();
        let now = Instant::now();
        for op in operations {
            assert!(admit(&mut budget, "a", "ip-a", op, now).is_ok());
            assert!(admit(&mut budget, "a", "ip-a", op, now).is_err());
        }
        assert!(admit(&mut budget, "b", "ip-b", RateBudgetOperation::NewOrder, now).is_ok());
        assert!(admit(&mut budget, "b", "ip-b", RateBudgetOperation::Cancel, now).is_err());
    }

    #[test]
    fn rate_budget_shared_egress_rejects_without_charging_other_account() {
        let mut specs = pair("a", "shared-ip", RateBudgetOperation::NewOrder, 1);
        specs.extend([
            spec(
                RateBudgetScope::Account(AccountId("b".into())),
                RateBudgetOperation::NewOrder,
                1,
            ),
            spec(
                RateBudgetScope::Egress("other-ip".into()),
                RateBudgetOperation::NewOrder,
                1,
            ),
        ]);
        let mut budget = RateBudget::new(specs).unwrap();
        let now = Instant::now();
        assert!(admit(
            &mut budget,
            "a",
            "shared-ip",
            RateBudgetOperation::NewOrder,
            now
        )
        .is_ok());
        assert_eq!(
            admit(
                &mut budget,
                "b",
                "shared-ip",
                RateBudgetOperation::NewOrder,
                now
            )
            .unwrap_err(),
            "egress rate budget exhausted for NewOrder"
        );
        assert!(admit(
            &mut budget,
            "b",
            "other-ip",
            RateBudgetOperation::NewOrder,
            now
        )
        .is_ok());
    }

    #[test]
    fn rate_budget_account_exhaustion_does_not_charge_shared_egress() {
        let mut specs = pair("a", "shared-ip", RateBudgetOperation::NewOrder, 1);
        specs[1].max_requests = 2;
        specs.push(spec(
            RateBudgetScope::Account(AccountId("b".into())),
            RateBudgetOperation::NewOrder,
            1,
        ));
        let mut budget = RateBudget::new(specs).unwrap();
        let now = Instant::now();
        assert!(admit(
            &mut budget,
            "a",
            "shared-ip",
            RateBudgetOperation::NewOrder,
            now
        )
        .is_ok());
        assert!(admit(
            &mut budget,
            "a",
            "shared-ip",
            RateBudgetOperation::NewOrder,
            now
        )
        .is_err());
        assert!(admit(
            &mut budget,
            "b",
            "shared-ip",
            RateBudgetOperation::NewOrder,
            now
        )
        .is_ok());
    }

    #[test]
    fn rate_budget_missing_dimensions_venue_and_product_fail_closed() {
        let mut budget =
            RateBudget::new(pair("a", "ip-a", RateBudgetOperation::NewOrder, 1)).unwrap();
        let account = AccountId("a".into());
        assert!(budget
            .admit(
                VenueId::BINANCE,
                ProductType::Spot,
                RateBudgetOperation::NewOrder,
                &account,
                "ip-a"
            )
            .is_err());
        assert!(budget
            .admit(
                VenueId::BYBIT,
                ProductType::Perp,
                RateBudgetOperation::NewOrder,
                &account,
                "ip-a"
            )
            .is_err());
        assert!(budget
            .admit(
                VenueId::BYBIT,
                ProductType::Spot,
                RateBudgetOperation::NewOrder,
                &account,
                "unknown-ip"
            )
            .is_err());
        assert!(budget
            .admit(
                VenueId::BYBIT,
                ProductType::Spot,
                RateBudgetOperation::NewOrder,
                &account,
                "ip-a"
            )
            .is_ok());
        assert!(RateBudget::default()
            .admit(
                VenueId::BYBIT,
                ProductType::Spot,
                RateBudgetOperation::NewOrder,
                &account,
                "ip-a"
            )
            .is_err());
    }

    #[test]
    fn rate_budget_invalid_configuration_is_rejected() {
        let mut specs = pair("a", "ip-a", RateBudgetOperation::NewOrder, 1);
        specs.push(specs[0].clone());
        assert!(RateBudget::new(specs).is_err());
        for invalid in [
            spec(
                RateBudgetScope::Egress(" ".into()),
                RateBudgetOperation::NewOrder,
                1,
            ),
            spec(
                RateBudgetScope::Account(AccountId("".into())),
                RateBudgetOperation::NewOrder,
                1,
            ),
            spec(
                RateBudgetScope::Egress("ip".into()),
                RateBudgetOperation::NewOrder,
                0,
            ),
            RateBudgetSpec {
                refill_interval: Duration::ZERO,
                ..spec(
                    RateBudgetScope::Egress("ip".into()),
                    RateBudgetOperation::NewOrder,
                    1,
                )
            },
        ] {
            assert!(RateBudget::new([invalid]).is_err());
        }
    }
}
