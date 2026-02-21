use crate::{Error, MerchantConfig, Subscription, SubscriptionStatus, SubscriptionVault, SubscriptionVaultClient};
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{token, Address, Env};

struct TestCtx {
    env: Env,
    contract_id: Address,
    token_address: Address,
    admin: Address,
    subscriber: Address,
}

impl TestCtx {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(SubscriptionVault, ());
        let client = SubscriptionVaultClient::new(&env, &contract_id);

        let token_admin = Address::generate(&env);
        let token_address = env
            .register_stellar_asset_contract_v2(token_admin)
            .address();
        let token_admin_client = token::StellarAssetClient::new(&env, &token_address);

        let admin = Address::generate(&env);
        let subscriber = Address::generate(&env);

        client.init(&token_address, &admin, &1_000000i128);
        token_admin_client.mint(&subscriber, &100_000000i128);

        Self {
            env,
            contract_id,
            token_address,
            admin,
            subscriber,
        }
    }

    fn client(&self) -> SubscriptionVaultClient<'_> {
        SubscriptionVaultClient::new(&self.env, &self.contract_id)
    }

    fn token_client(&self) -> token::Client<'_> {
        token::Client::new(&self.env, &self.token_address)
    }

    fn stellar_asset_client(&self) -> token::StellarAssetClient<'_> {
        token::StellarAssetClient::new(&self.env, &self.token_address)
    }

    fn mint_to(&self, recipient: &Address, amount: i128) {
        self.stellar_asset_client().mint(recipient, &amount);
    }

    fn approve_vault_spend(&self, subscriber: &Address, amount: i128) {
        self.token_client().approve(
            subscriber,
            &self.contract_id,
            &amount,
            &self.env.ledger().sequence().saturating_add(500),
        );
    }

    fn create_subscription_for(
        &self,
        subscriber: &Address,
        merchant: &Address,
        amount: i128,
    ) -> u32 {
        self.approve_vault_spend(subscriber, amount);
        self.client()
            .create_subscription(subscriber, merchant, &amount, &3600u64, &false)
    }
}

#[test]
fn test_init_and_struct() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    assert_eq!(client.get_min_topup(), 1_000000i128);

    let sub = Subscription {
        subscriber: Address::generate(&ctx.env),
        merchant: Address::generate(&ctx.env),
        amount: 10_000_0000,
        interval_seconds: 30 * 24 * 60 * 60,
        last_payment_timestamp: 0,
        status: SubscriptionStatus::Active,
        prepaid_balance: 50_000_0000,
        usage_enabled: false,
    };
    assert_eq!(sub.status, SubscriptionStatus::Active);
}

#[test]
fn test_create_subscription_initializes_prepaid_and_transfers_tokens() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let token_client = ctx.token_client();
    let merchant = Address::generate(&ctx.env);
    let amount = 10_000000i128;

    ctx.approve_vault_spend(&ctx.subscriber, amount);

    let before_subscriber = token_client.balance(&ctx.subscriber);
    let before_vault = token_client.balance(&ctx.contract_id);

    let sub_id = client.create_subscription(&ctx.subscriber, &merchant, &amount, &3600u64, &false);
    let sub = client.get_subscription(&sub_id);

    assert_eq!(sub.prepaid_balance, amount);
    assert_eq!(sub.last_payment_timestamp, ctx.env.ledger().timestamp());
    assert_eq!(sub.status, SubscriptionStatus::Active);
    assert_eq!(token_client.balance(&ctx.subscriber), before_subscriber - amount);
    assert_eq!(token_client.balance(&ctx.contract_id), before_vault + amount);
}

#[test]
fn test_create_subscription_missing_allowance_fails() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);

    let result =
        client.try_create_subscription(&ctx.subscriber, &merchant, &5_000000i128, &3600u64, &false);
    assert_eq!(result, Err(Ok(Error::InsufficientAllowance)));
}

#[test]
fn test_create_subscription_transfer_failure_on_low_balance() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);
    let amount = 500_000000i128;

    ctx.approve_vault_spend(&ctx.subscriber, amount);

    let result = client.try_create_subscription(&ctx.subscriber, &merchant, &amount, &3600u64, &true);
    assert_eq!(result, Err(Ok(Error::TransferFailed)));
}

#[test]
fn test_create_subscription_zero_or_negative_amount_fails() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);

    ctx.approve_vault_spend(&ctx.subscriber, 1_000000i128);

    let zero = client.try_create_subscription(&ctx.subscriber, &merchant, &0i128, &3600u64, &false);
    let negative = client.try_create_subscription(&ctx.subscriber, &merchant, &-1i128, &3600u64, &false);

    assert_eq!(zero, Err(Ok(Error::InvalidAmount)));
    assert_eq!(negative, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_create_subscription_zero_interval_fails() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);
    let amount = 1_000000i128;

    ctx.approve_vault_spend(&ctx.subscriber, amount);

    let result = client.try_create_subscription(&ctx.subscriber, &merchant, &amount, &0u64, &false);
    assert_eq!(result, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_get_merchant_config_returns_defaults_when_unset() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);

    let config = client.get_merchant_config(&merchant);
    assert_eq!(
        config,
        MerchantConfig {
            version: 1,
            min_subscription_amount: 0,
            default_interval_seconds: 0,
        }
    );
}

#[test]
fn test_set_merchant_config_by_merchant_and_apply_defaults_on_create() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);
    let subscriber = Address::generate(&ctx.env);
    ctx.mint_to(&subscriber, 30_000000i128);

    client.set_merchant_config(&merchant, &merchant, &5_000000i128, &7200u64);
    let stored = client.get_merchant_config(&merchant);
    assert_eq!(stored.min_subscription_amount, 5_000000i128);
    assert_eq!(stored.default_interval_seconds, 7200u64);

    ctx.approve_vault_spend(&subscriber, 6_000000i128);
    let sub_id = client.create_subscription(&subscriber, &merchant, &6_000000i128, &0u64, &false);
    let sub = client.get_subscription(&sub_id);
    assert_eq!(sub.interval_seconds, 7200u64);

    ctx.approve_vault_spend(&subscriber, 4_000000i128);
    let below_min =
        client.try_create_subscription(&subscriber, &merchant, &4_000000i128, &0u64, &false);
    assert_eq!(below_min, Err(Ok(Error::BelowMerchantMinimum)));
}

#[test]
fn test_set_and_update_merchant_config_by_admin_over_time() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);
    let subscriber = Address::generate(&ctx.env);
    ctx.mint_to(&subscriber, 50_000000i128);

    client.set_merchant_config(&ctx.admin, &merchant, &3_000000i128, &3600u64);
    let first = client.get_merchant_config(&merchant);
    assert_eq!(first.min_subscription_amount, 3_000000i128);
    assert_eq!(first.default_interval_seconds, 3600u64);

    client.update_merchant_config(&ctx.admin, &merchant, &None, &Some(1800u64));
    let updated = client.get_merchant_config(&merchant);
    assert_eq!(updated.min_subscription_amount, 3_000000i128);
    assert_eq!(updated.default_interval_seconds, 1800u64);

    ctx.approve_vault_spend(&subscriber, 3_500000i128);
    let sub_id = client.create_subscription(&subscriber, &merchant, &3_500000i128, &0u64, &false);
    assert_eq!(client.get_subscription(&sub_id).interval_seconds, 1800u64);
}

#[test]
fn test_merchant_config_unauthorized_and_invalid_update_failures() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);
    let attacker = Address::generate(&ctx.env);

    let unauthorized_set =
        client.try_set_merchant_config(&attacker, &merchant, &1_000000i128, &3600u64);
    assert_eq!(unauthorized_set, Err(Ok(Error::Unauthorized)));

    client.set_merchant_config(&merchant, &merchant, &1_000000i128, &3600u64);
    let bad_update =
        client.try_update_merchant_config(&merchant, &merchant, &Some(-1i128), &None);
    assert_eq!(bad_update, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_charge_subscription_credits_shared_merchant_balance_multiple_subscribers() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);

    let subscriber_a = Address::generate(&ctx.env);
    let subscriber_b = Address::generate(&ctx.env);
    ctx.mint_to(&subscriber_a, 30_000000i128);
    ctx.mint_to(&subscriber_b, 40_000000i128);

    let sub_a = ctx.create_subscription_for(&subscriber_a, &merchant, 12_000000i128);
    let sub_b = ctx.create_subscription_for(&subscriber_b, &merchant, 25_000000i128);

    client.charge_subscription(&sub_a);
    client.charge_subscription(&sub_b);

    assert_eq!(client.get_merchant_balance(&merchant), 37_000000i128);
    assert_eq!(client.get_subscription(&sub_a).prepaid_balance, 0i128);
    assert_eq!(client.get_subscription(&sub_b).prepaid_balance, 0i128);
}

#[test]
fn test_charge_subscription_insufficient_prepaid_does_not_credit() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);

    let sub_id = ctx.create_subscription_for(&ctx.subscriber, &merchant, 10_000000i128);
    client.charge_subscription(&sub_id);

    let second_charge = client.try_charge_subscription(&sub_id);
    assert_eq!(second_charge, Err(Ok(Error::InsufficientBalance)));
    assert_eq!(client.get_merchant_balance(&merchant), 10_000000i128);
    assert_eq!(client.get_subscription(&sub_id).status, SubscriptionStatus::Active);
}

#[test]
fn test_merchant_balances_are_isolated_across_merchants() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let subscriber_two = Address::generate(&ctx.env);
    ctx.mint_to(&subscriber_two, 20_000000i128);

    let merchant_a = Address::generate(&ctx.env);
    let merchant_b = Address::generate(&ctx.env);

    let sub_a = ctx.create_subscription_for(&ctx.subscriber, &merchant_a, 7_000000i128);
    let sub_b = ctx.create_subscription_for(&subscriber_two, &merchant_b, 13_000000i128);

    client.charge_subscription(&sub_a);
    client.charge_subscription(&sub_b);

    assert_eq!(client.get_merchant_balance(&merchant_a), 7_000000i128);
    assert_eq!(client.get_merchant_balance(&merchant_b), 13_000000i128);
}

#[test]
fn test_withdraw_merchant_funds_debits_internal_balance_and_transfers_tokens() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let token_client = ctx.token_client();
    let merchant = Address::generate(&ctx.env);

    let sub_id = ctx.create_subscription_for(&ctx.subscriber, &merchant, 20_000000i128);
    client.charge_subscription(&sub_id);

    let before_wallet = token_client.balance(&merchant);
    client.withdraw_merchant_funds(&merchant, &8_000000i128);

    assert_eq!(client.get_merchant_balance(&merchant), 12_000000i128);
    assert_eq!(token_client.balance(&merchant), before_wallet + 8_000000i128);
}

#[test]
fn test_withdraw_merchant_funds_prevents_double_spend() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);

    let sub_id = ctx.create_subscription_for(&ctx.subscriber, &merchant, 9_000000i128);
    client.charge_subscription(&sub_id);

    client.withdraw_merchant_funds(&merchant, &9_000000i128);
    assert_eq!(client.get_merchant_balance(&merchant), 0i128);

    let second_withdraw = client.try_withdraw_merchant_funds(&merchant, &1_000000i128);
    assert_eq!(second_withdraw, Err(Ok(Error::InsufficientBalance)));
}

#[test]
fn test_large_balance_accumulation_for_single_merchant() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let merchant = Address::generate(&ctx.env);

    let s1 = Address::generate(&ctx.env);
    let s2 = Address::generate(&ctx.env);
    let s3 = Address::generate(&ctx.env);
    let chunk = 2_000_000_000i128;

    ctx.mint_to(&s1, chunk + 1_000000i128);
    ctx.mint_to(&s2, chunk + 1_000000i128);
    ctx.mint_to(&s3, chunk + 1_000000i128);

    let sub1 = ctx.create_subscription_for(&s1, &merchant, chunk);
    let sub2 = ctx.create_subscription_for(&s2, &merchant, chunk);
    let sub3 = ctx.create_subscription_for(&s3, &merchant, chunk);

    client.charge_subscription(&sub1);
    client.charge_subscription(&sub2);
    client.charge_subscription(&sub3);

    assert_eq!(client.get_merchant_balance(&merchant), chunk * 3);
}

#[test]
fn test_min_topup_below_threshold() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    client.set_min_topup(&ctx.admin, &5_000000i128);

    let below = client.try_deposit_funds(&0, &ctx.subscriber, &4_999999i128);
    assert_eq!(below, Err(Ok(Error::BelowMinimumTopup)));

    let zero = client.try_deposit_funds(&0, &ctx.subscriber, &0i128);
    assert_eq!(zero, Err(Ok(Error::InvalidAmount)));

    let negative = client.try_deposit_funds(&0, &ctx.subscriber, &-100i128);
    assert_eq!(negative, Err(Ok(Error::InvalidAmount)));
}

#[test]
fn test_min_topup_exactly_at_threshold() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let min_topup = 5_000000i128;
    client.set_min_topup(&ctx.admin, &min_topup);

    let result = client.try_deposit_funds(&0, &ctx.subscriber, &min_topup);
    assert!(result.is_ok());
}

#[test]
fn test_min_topup_above_threshold() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    client.set_min_topup(&ctx.admin, &5_000000i128);

    let result = client.try_deposit_funds(&0, &ctx.subscriber, &10_000000i128);
    assert!(result.is_ok());
}

#[test]
fn test_set_min_topup_by_admin() {
    let ctx = TestCtx::new();
    let client = ctx.client();

    assert_eq!(client.get_min_topup(), 1_000000i128);

    let new_min = 10_000000i128;
    client.set_min_topup(&ctx.admin, &new_min);
    assert_eq!(client.get_min_topup(), new_min);
}

#[test]
fn test_set_min_topup_unauthorized() {
    let ctx = TestCtx::new();
    let client = ctx.client();
    let non_admin = Address::generate(&ctx.env);

    let result = client.try_set_min_topup(&non_admin, &5_000000i128);
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

#[test]
fn test_invalid_min_topup_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let token_admin = Address::generate(&env);
    let token_address = env
        .register_stellar_asset_contract_v2(token_admin)
        .address();
    let admin = Address::generate(&env);

    assert_eq!(
        client.try_init(&token_address, &admin, &0i128),
        Err(Ok(Error::InvalidAmount))
    );
    client.init(&token_address, &admin, &1_000000i128);
    assert_eq!(
        client.try_set_min_topup(&admin, &0i128),
        Err(Ok(Error::InvalidAmount))
    );
}
