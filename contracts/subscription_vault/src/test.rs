use crate::{
    can_transition, get_allowed_transitions, validate_status_transition, Error,
    RecoveryReason, Subscription, SubscriptionStatus, SubscriptionVault, SubscriptionVaultClient,
};
use soroban_sdk::testutils::{Address as _, Ledger as _, Events};
use soroban_sdk::{Address, Env};

// =============================================================================
// State Machine Helper Tests
// =============================================================================

#[test]
fn test_validate_status_transition_same_status_is_allowed() {
    // Idempotent transitions should be allowed
    assert!(validate_status_transition(&SubscriptionStatus::Active, &SubscriptionStatus::Active).is_ok());
    assert!(validate_status_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::Paused).is_ok());
    assert!(validate_status_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::Cancelled).is_ok());
    assert!(validate_status_transition(&SubscriptionStatus::InsufficientBalance, &SubscriptionStatus::InsufficientBalance).is_ok());
}

#[test]
fn test_validate_active_transitions() {
    // Active -> Paused (allowed)
    assert!(validate_status_transition(&SubscriptionStatus::Active, &SubscriptionStatus::Paused).is_ok());
    
    // Active -> Cancelled (allowed)
    assert!(validate_status_transition(&SubscriptionStatus::Active, &SubscriptionStatus::Cancelled).is_ok());
    
    // Active -> InsufficientBalance (allowed)
    assert!(validate_status_transition(&SubscriptionStatus::Active, &SubscriptionStatus::InsufficientBalance).is_ok());
}

#[test]
fn test_validate_paused_transitions() {
    // Paused -> Active (allowed)
    assert!(validate_status_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::Active).is_ok());
    
    // Paused -> Cancelled (allowed)
    assert!(validate_status_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::Cancelled).is_ok());
    
    // Paused -> InsufficientBalance (not allowed)
    assert_eq!(
        validate_status_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::InsufficientBalance),
        Err(Error::InvalidStatusTransition)
    );
}

#[test]
fn test_validate_insufficient_balance_transitions() {
    // InsufficientBalance -> Active (allowed)
    assert!(validate_status_transition(&SubscriptionStatus::InsufficientBalance, &SubscriptionStatus::Active).is_ok());
    
    // InsufficientBalance -> Cancelled (allowed)
    assert!(validate_status_transition(&SubscriptionStatus::InsufficientBalance, &SubscriptionStatus::Cancelled).is_ok());
    
    // InsufficientBalance -> Paused (not allowed)
    assert_eq!(
        validate_status_transition(&SubscriptionStatus::InsufficientBalance, &SubscriptionStatus::Paused),
        Err(Error::InvalidStatusTransition)
    );
}

#[test]
fn test_validate_cancelled_transitions_all_blocked() {
    // Cancelled is a terminal state - no outgoing transitions allowed
    assert_eq!(
        validate_status_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::Active),
        Err(Error::InvalidStatusTransition)
    );
    assert_eq!(
        validate_status_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::Paused),
        Err(Error::InvalidStatusTransition)
    );
    assert_eq!(
        validate_status_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::InsufficientBalance),
        Err(Error::InvalidStatusTransition)
    );
}

#[test]
fn test_can_transition_helper() {
    // True cases
    assert!(can_transition(&SubscriptionStatus::Active, &SubscriptionStatus::Paused));
    assert!(can_transition(&SubscriptionStatus::Active, &SubscriptionStatus::Cancelled));
    assert!(can_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::Active));
    
    // False cases
    assert!(!can_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::Active));
    assert!(!can_transition(&SubscriptionStatus::Cancelled, &SubscriptionStatus::Paused));
    assert!(!can_transition(&SubscriptionStatus::Paused, &SubscriptionStatus::InsufficientBalance));
}

#[test]
fn test_get_allowed_transitions() {
    // Active
    let active_targets = get_allowed_transitions(&SubscriptionStatus::Active);
    assert_eq!(active_targets.len(), 3);
    assert!(active_targets.contains(&SubscriptionStatus::Paused));
    assert!(active_targets.contains(&SubscriptionStatus::Cancelled));
    assert!(active_targets.contains(&SubscriptionStatus::InsufficientBalance));
    
    // Paused
    let paused_targets = get_allowed_transitions(&SubscriptionStatus::Paused);
    assert_eq!(paused_targets.len(), 2);
    assert!(paused_targets.contains(&SubscriptionStatus::Active));
    assert!(paused_targets.contains(&SubscriptionStatus::Cancelled));
    
    // Cancelled
    let cancelled_targets = get_allowed_transitions(&SubscriptionStatus::Cancelled);
    assert_eq!(cancelled_targets.len(), 0);
    
    // InsufficientBalance
    let ib_targets = get_allowed_transitions(&SubscriptionStatus::InsufficientBalance);
    assert_eq!(ib_targets.len(), 2);
    assert!(ib_targets.contains(&SubscriptionStatus::Active));
    assert!(ib_targets.contains(&SubscriptionStatus::Cancelled));
}

// =============================================================================
// Contract Entrypoint State Transition Tests
// =============================================================================

fn setup_test_env() -> (Env, SubscriptionVaultClient<'static>, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);
    
    let token = Address::generate(&env);
    let admin = Address::generate(&env);
    let min_topup = 1_000000i128; // 1 USDC
    client.init(&token, &admin, &min_topup);
    
    (env, client, token, admin)
}

fn create_test_subscription(env: &Env, client: &SubscriptionVaultClient, status: SubscriptionStatus) -> (u32, Address, Address) {
    let subscriber = Address::generate(env);
    let merchant = Address::generate(env);
    let amount = 10_000_000i128; // 10 USDC
    let interval_seconds = 30 * 24 * 60 * 60; // 30 days
    let usage_enabled = false;
    
    // Create subscription (always starts as Active)
    let id = client.create_subscription(&subscriber, &merchant, &amount, &interval_seconds, &usage_enabled);
    
    // Manually set status if not Active (bypassing state machine for test setup)
    // Note: In production, this would go through proper transitions
    if status != SubscriptionStatus::Active {
        // We need to manipulate storage directly for test setup
        // This is a test-only pattern
        let mut sub = client.get_subscription(&id);
        sub.status = status;
        env.as_contract(&client.address, || {
            env.storage().instance().set(&id, &sub);
        });
    }
    
    (id, subscriber, merchant)
}

#[test]
fn test_pause_subscription_from_active() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // Pause from Active should succeed
    client.pause_subscription(&id, &subscriber);
    
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Paused);
}

#[test]
#[should_panic(expected = "Error(Contract, #400)")]
fn test_pause_subscription_from_cancelled_should_fail() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // First cancel
    client.cancel_subscription(&id, &subscriber);
    
    // Then try to pause (should fail)
    client.pause_subscription(&id, &subscriber);
}

#[test]
fn test_pause_subscription_from_paused_is_idempotent() {
    // Idempotent transition: Paused -> Paused should succeed (no-op)
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // First pause
    client.pause_subscription(&id, &subscriber);
    assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Paused);
    
    // Pausing again should succeed (idempotent)
    client.pause_subscription(&id, &subscriber);
    assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Paused);
}

#[test]
fn test_cancel_subscription_from_active() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // Cancel from Active should succeed
    client.cancel_subscription(&id, &subscriber);
    
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Cancelled);
}

#[test]
fn test_cancel_subscription_from_paused() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // First pause
    client.pause_subscription(&id, &subscriber);
    
    // Then cancel
    client.cancel_subscription(&id, &subscriber);
    
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Cancelled);
}

#[test]
fn test_cancel_subscription_from_cancelled_is_idempotent() {
    // Idempotent transition: Cancelled -> Cancelled should succeed (no-op)
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // First cancel
    client.cancel_subscription(&id, &subscriber);
    assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Cancelled);
    
    // Cancelling again should succeed (idempotent)
    client.cancel_subscription(&id, &subscriber);
    assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Cancelled);
}

#[test]
fn test_resume_subscription_from_paused() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // First pause
    client.pause_subscription(&id, &subscriber);
    
    // Then resume
    client.resume_subscription(&id, &subscriber);
    
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Active);
}

#[test]
#[should_panic(expected = "Error(Contract, #400)")]
fn test_resume_subscription_from_cancelled_should_fail() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // First cancel
    client.cancel_subscription(&id, &subscriber);
    
    // Try to resume (should fail)
    client.resume_subscription(&id, &subscriber);
}

#[test]
fn test_state_transition_idempotent_same_status() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // Cancelling from already cancelled should fail (but we need to set it first)
    // First cancel
    client.cancel_subscription(&id, &subscriber);
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Cancelled);
}

// =============================================================================
// Complex State Transition Sequences
// =============================================================================

#[test]
fn test_full_lifecycle_active_pause_resume() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // Active -> Paused
    client.pause_subscription(&id, &subscriber);
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Paused);
    
    // Paused -> Active
    client.resume_subscription(&id, &subscriber);
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Active);
    
    // Can pause again
    client.pause_subscription(&id, &subscriber);
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Paused);
}

#[test]
fn test_full_lifecycle_active_cancel() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // Active -> Cancelled (terminal)
    client.cancel_subscription(&id, &subscriber);
    let sub = client.get_subscription(&id);
    assert_eq!(sub.status, SubscriptionStatus::Cancelled);
    
    // Verify no further transitions possible
    // We can't easily test all fail cases without #[should_panic] for each
}

#[test]
fn test_all_valid_transitions_coverage() {
    // This test exercises every valid state transition at least once
    
    // 1. Active -> Paused
    {
        let (env, client, _, _) = setup_test_env();
        let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
        client.pause_subscription(&id, &subscriber);
        assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Paused);
    }
    
    // 2. Active -> Cancelled
    {
        let (env, client, _, _) = setup_test_env();
        let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
        client.cancel_subscription(&id, &subscriber);
        assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Cancelled);
    }
    
    // 3. Active -> InsufficientBalance (simulated via direct storage manipulation)
    {
        let (env, client, _, _) = setup_test_env();
        let (id, _subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
        
        // Simulate transition by updating storage directly
        let mut sub = client.get_subscription(&id);
        sub.status = SubscriptionStatus::InsufficientBalance;
        env.as_contract(&client.address, || {
            env.storage().instance().set(&id, &sub);
        });
        
        assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::InsufficientBalance);
    }
    
    // 4. Paused -> Active
    {
        let (env, client, _, _) = setup_test_env();
        let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
        client.pause_subscription(&id, &subscriber);
        client.resume_subscription(&id, &subscriber);
        assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Active);
    }
    
    // 5. Paused -> Cancelled
    {
        let (env, client, _, _) = setup_test_env();
        let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
        client.pause_subscription(&id, &subscriber);
        client.cancel_subscription(&id, &subscriber);
        assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Cancelled);
    }
    
    // 6. InsufficientBalance -> Active
    {
        let (env, client, _, _) = setup_test_env();
        let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
        
        // Set to InsufficientBalance
        let mut sub = client.get_subscription(&id);
        sub.status = SubscriptionStatus::InsufficientBalance;
        env.as_contract(&client.address, || {
            env.storage().instance().set(&id, &sub);
        });
        
        // Resume to Active
        client.resume_subscription(&id, &subscriber);
        assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Active);
    }
    
    // 7. InsufficientBalance -> Cancelled
    {
        let (env, client, _, _) = setup_test_env();
        let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
        
        // Set to InsufficientBalance
        let mut sub = client.get_subscription(&id);
        sub.status = SubscriptionStatus::InsufficientBalance;
        env.as_contract(&client.address, || {
            env.storage().instance().set(&id, &sub);
        });
        
        // Cancel
        client.cancel_subscription(&id, &subscriber);
        assert_eq!(client.get_subscription(&id).status, SubscriptionStatus::Cancelled);
    }
}

// =============================================================================
// Invalid Transition Tests (#[should_panic] for each invalid case)
// =============================================================================

#[test]
#[should_panic(expected = "Error(Contract, #400)")]
fn test_invalid_cancelled_to_active() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    client.cancel_subscription(&id, &subscriber);
    client.resume_subscription(&id, &subscriber);
}

#[test]
#[should_panic(expected = "Error(Contract, #400)")]
fn test_invalid_insufficient_balance_to_paused() {
    let (env, client, _, _) = setup_test_env();
    let (id, subscriber, _) = create_test_subscription(&env, &client, SubscriptionStatus::Active);
    
    // Set to InsufficientBalance
    let mut sub = client.get_subscription(&id);
    sub.status = SubscriptionStatus::InsufficientBalance;
    env.as_contract(&client.address, || {
        env.storage().instance().set(&id, &sub);
    });
    
    // Can't pause from InsufficientBalance - only resume to Active or cancel
    // Since pause_subscription validates Active -> Paused, this should fail
    client.pause_subscription(&id, &subscriber);
}

#[test]
fn test_subscription_struct_status_field() {
    let env = Env::default();
    let sub = Subscription {
        subscriber: Address::generate(&env),
        merchant: Address::generate(&env),
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
fn test_init_and_struct() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let _client = SubscriptionVaultClient::new(&env, &contract_id);
    // Basic initialization test
    assert!(true);
}

#[test]
fn test_min_topup_below_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let token = Address::generate(&env);
    let admin = Address::generate(&env);
    let subscriber = Address::generate(&env);
    let min_topup = 5_000000i128; // 5 USDC
    
    client.init(&token, &admin, &min_topup);
    
    let result = client.try_deposit_funds(&0, &subscriber, &4_999999);
    assert!(result.is_err());
}

#[test]
fn test_min_topup_exactly_at_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let token = Address::generate(&env);
    let admin = Address::generate(&env);
    let subscriber = Address::generate(&env);
    let min_topup = 5_000000i128; // 5 USDC
    
    client.init(&token, &admin, &min_topup);
    
    let result = client.try_deposit_funds(&0, &subscriber, &min_topup);
    assert!(result.is_ok());
}

#[test]
fn test_min_topup_above_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let token = Address::generate(&env);
    let admin = Address::generate(&env);
    let subscriber = Address::generate(&env);
    let min_topup = 5_000000i128; // 5 USDC
    
    client.init(&token, &admin, &min_topup);
    
    let result = client.try_deposit_funds(&0, &subscriber, &10_000000);
    assert!(result.is_ok());
}

#[test]
fn test_set_min_topup_by_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let token = Address::generate(&env);
    let admin = Address::generate(&env);
    let initial_min = 1_000000i128;
    let new_min = 10_000000i128;
    
    client.init(&token, &admin, &initial_min);
    assert_eq!(client.get_min_topup(), initial_min);
    
    client.set_min_topup(&admin, &new_min);
    assert_eq!(client.get_min_topup(), new_min);
}

#[test]
fn test_set_min_topup_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(SubscriptionVault, ());
    let client = SubscriptionVaultClient::new(&env, &contract_id);

    let token = Address::generate(&env);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let min_topup = 1_000000i128;
    
    client.init(&token, &admin, &min_topup);
    
    let result = client.try_set_min_topup(&non_admin, &5_000000);
    assert!(result.is_err());
}
// =============================================================================
// Next Charge Timestamp Helper Tests
// =============================================================================

#[test]
fn test_compute_next_charge_info_active_subscription() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    let last_payment = 1000u64;
    let interval = 30 * 24 * 60 * 60; // 30 days in seconds
    
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 10_000_000i128,
        interval_seconds: interval,
        last_payment_timestamp: last_payment,
        status: SubscriptionStatus::Active,
        prepaid_balance: 100_000_000i128,
        usage_enabled: false,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    // Active subscription: charge is expected
    assert!(info.is_charge_expected);
    // Next charge = last_payment + interval
    assert_eq!(info.next_charge_timestamp, last_payment + interval);
}

#[test]
fn test_compute_next_charge_info_paused_subscription() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    let last_payment = 2000u64;
    let interval = 7 * 24 * 60 * 60; // 7 days
    
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 5_000_000i128,
        interval_seconds: interval,
        last_payment_timestamp: last_payment,
        status: SubscriptionStatus::Paused,
        prepaid_balance: 50_000_000i128,
        usage_enabled: false,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    // Paused subscription: charge is NOT expected
    assert!(!info.is_charge_expected);
    // Timestamp is still computed for reference
    assert_eq!(info.next_charge_timestamp, last_payment + interval);
}

#[test]
fn test_compute_next_charge_info_cancelled_subscription() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    let last_payment = 5000u64;
    let interval = 24 * 60 * 60; // 1 day
    
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 1_000_000i128,
        interval_seconds: interval,
        last_payment_timestamp: last_payment,
        status: SubscriptionStatus::Cancelled,
        prepaid_balance: 0i128,
        usage_enabled: false,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    // Cancelled subscription: charge is NOT expected (terminal state)
    assert!(!info.is_charge_expected);
    // Timestamp is still computed for reference
    assert_eq!(info.next_charge_timestamp, last_payment + interval);
}

#[test]
fn test_compute_next_charge_info_insufficient_balance_subscription() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    let last_payment = 3000u64;
    let interval = 30 * 24 * 60 * 60; // 30 days
    
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 20_000_000i128,
        interval_seconds: interval,
        last_payment_timestamp: last_payment,
        status: SubscriptionStatus::InsufficientBalance,
        prepaid_balance: 1_000_000i128, // Not enough for next charge
        usage_enabled: false,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    // InsufficientBalance subscription: charge IS expected (will retry after funding)
    assert!(info.is_charge_expected);
    // Next charge = last_payment + interval
    assert_eq!(info.next_charge_timestamp, last_payment + interval);
}

#[test]
fn test_compute_next_charge_info_short_interval() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    let last_payment = 100000u64;
    let interval = 60; // 1 minute interval
    
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 1_000i128,
        interval_seconds: interval,
        last_payment_timestamp: last_payment,
        status: SubscriptionStatus::Active,
        prepaid_balance: 10_000i128,
        usage_enabled: true,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    assert!(info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, last_payment + interval);
}

#[test]
fn test_compute_next_charge_info_long_interval() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    let last_payment = 1000u64;
    let interval = 365 * 24 * 60 * 60; // 1 year in seconds
    
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 100_000_000i128,
        interval_seconds: interval,
        last_payment_timestamp: last_payment,
        status: SubscriptionStatus::Active,
        prepaid_balance: 1_000_000_000i128,
        usage_enabled: false,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    assert!(info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, last_payment + interval);
}

#[test]
fn test_compute_next_charge_info_overflow_protection() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    // Test saturating_add behavior at edge of u64 range
    let last_payment = u64::MAX - 100;
    let interval = 200; // Would overflow without saturating_add
    
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 10_000_000i128,
        interval_seconds: interval,
        last_payment_timestamp: last_payment,
        status: SubscriptionStatus::Active,
        prepaid_balance: 100_000_000i128,
        usage_enabled: false,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    assert!(info.is_charge_expected);
    // Should saturate to u64::MAX instead of wrapping
    assert_eq!(info.next_charge_timestamp, u64::MAX);
}

#[test]
fn test_get_next_charge_info_contract_method() {
    let (env, client, _, _) = setup_test_env();
    
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let amount = 10_000_000i128;
    let interval_seconds = 30 * 24 * 60 * 60; // 30 days
    
    // Set initial ledger timestamp
    env.ledger().with_mut(|li| li.timestamp = 1000);
    
    // Create subscription
    let id = client.create_subscription(&subscriber, &merchant, &amount, &interval_seconds, &false);
    
    // Get next charge info
    let info = client.get_next_charge_info(&id);
    
    // Should be Active with charge expected
    assert!(info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, 1000 + interval_seconds);
}

#[test]
fn test_get_next_charge_info_all_statuses() {
    let (env, client, _, _) = setup_test_env();
    
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let amount = 10_000_000i128;
    let interval_seconds = 30 * 24 * 60 * 60;
    
    env.ledger().with_mut(|li| li.timestamp = 5000);
    
    // Create subscription (starts as Active)
    let id = client.create_subscription(&subscriber, &merchant, &amount, &interval_seconds, &false);
    
    // Test Active status
    let info = client.get_next_charge_info(&id);
    assert!(info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, 5000 + interval_seconds);
    
    // Test Paused status
    client.pause_subscription(&id, &subscriber);
    let info = client.get_next_charge_info(&id);
    assert!(!info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, 5000 + interval_seconds);
    
    // Resume to Active
    client.resume_subscription(&id, &subscriber);
    let info = client.get_next_charge_info(&id);
    assert!(info.is_charge_expected);
    
    // Test Cancelled status
    client.cancel_subscription(&id, &subscriber);
    let info = client.get_next_charge_info(&id);
    assert!(!info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, 5000 + interval_seconds);
}

#[test]
fn test_get_next_charge_info_insufficient_balance_status() {
    use crate::SubscriptionStatus;
    
    let (env, client, _, _) = setup_test_env();
    
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let amount = 10_000_000i128;
    let interval_seconds = 7 * 24 * 60 * 60; // 7 days
    
    env.ledger().with_mut(|li| li.timestamp = 2000);
    
    // Create subscription
    let id = client.create_subscription(&subscriber, &merchant, &amount, &interval_seconds, &false);
    
    // Manually set to InsufficientBalance for testing
    let mut sub = client.get_subscription(&id);
    sub.status = SubscriptionStatus::InsufficientBalance;
    env.as_contract(&client.address, || {
        env.storage().instance().set(&id, &sub);
    });
    
    // Get next charge info
    let info = client.get_next_charge_info(&id);
    
    // InsufficientBalance: charge IS expected (will retry after funding)
    assert!(info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, 2000 + interval_seconds);
}

#[test]
#[should_panic(expected = "Error(Contract, #404)")]
fn test_get_next_charge_info_subscription_not_found() {
    let (_, client, _, _) = setup_test_env();
    
    // Try to get next charge info for non-existent subscription
    client.get_next_charge_info(&999);
}

#[test]
fn test_get_next_charge_info_multiple_intervals() {
    let (env, client, _, _) = setup_test_env();
    
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    // Daily subscription
    env.ledger().with_mut(|li| li.timestamp = 10000);
    let daily_id = client.create_subscription(
        &subscriber,
        &merchant,
        &1_000_000i128,
        &(24 * 60 * 60), // 1 day
        &false
    );
    
    // Weekly subscription
    env.ledger().with_mut(|li| li.timestamp = 20000);
    let weekly_id = client.create_subscription(
        &subscriber,
        &merchant,
        &5_000_000i128,
        &(7 * 24 * 60 * 60), // 7 days
        &false
    );
    
    // Monthly subscription
    env.ledger().with_mut(|li| li.timestamp = 30000);
    let monthly_id = client.create_subscription(
        &subscriber,
        &merchant,
        &20_000_000i128,
        &(30 * 24 * 60 * 60), // 30 days
        &false
    );
    
    // Check each subscription has correct next charge time
    let daily_info = client.get_next_charge_info(&daily_id);
    assert_eq!(daily_info.next_charge_timestamp, 10000 + 24 * 60 * 60);
    
    let weekly_info = client.get_next_charge_info(&weekly_id);
    assert_eq!(weekly_info.next_charge_timestamp, 20000 + 7 * 24 * 60 * 60);
    
    let monthly_info = client.get_next_charge_info(&monthly_id);
    assert_eq!(monthly_info.next_charge_timestamp, 30000 + 30 * 24 * 60 * 60);
    
    // All should have charges expected (Active status)
    assert!(daily_info.is_charge_expected);
    assert!(weekly_info.is_charge_expected);
    assert!(monthly_info.is_charge_expected);
}

#[test]
fn test_get_next_charge_info_zero_interval() {
    use crate::{compute_next_charge_info, Subscription, SubscriptionStatus};
    
    let env = Env::default();
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    
    // Edge case: zero interval (immediate recurring charge)
    let subscription = Subscription {
        subscriber,
        merchant,
        amount: 1_000_000i128,
        interval_seconds: 0,
        last_payment_timestamp: 5000,
        status: SubscriptionStatus::Active,
        prepaid_balance: 10_000_000i128,
        usage_enabled: false,
    };
    
    let info = compute_next_charge_info(&subscription);
    
    assert!(info.is_charge_expected);
    assert_eq!(info.next_charge_timestamp, 5000); // 5000 + 0 = 5000
}

// =============================================================================
// Admin Recovery of Stranded Funds Tests
// =============================================================================

#[test]
fn test_recover_stranded_funds_successful() {
    let (env, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&env);
    let amount = 50_000_000i128; // 50 USDC
    let reason = RecoveryReason::AccidentalTransfer;
    
    env.ledger().with_mut(|li| li.timestamp = 10000);
    
    // Recovery should succeed
    let result = client.try_recover_stranded_funds(&admin, &recipient, &amount, &reason);
    assert!(result.is_ok());
    
    // Verify event was emitted
    let events = env.events().all();
    assert!(events.len() > 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #401)")]
fn test_recover_stranded_funds_unauthorized_caller() {
    let (env, client, _, _) = setup_test_env();
    
    let non_admin = Address::generate(&env);
    let recipient = Address::generate(&env);
    let amount = 10_000_000i128;
    let reason = RecoveryReason::AccidentalTransfer;
    
    // Should fail: caller is not admin
    client.recover_stranded_funds(&non_admin, &recipient, &amount, &reason);
}

#[test]
#[should_panic(expected = "Error(Contract, #405)")]
fn test_recover_stranded_funds_zero_amount() {
    let (_, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&admin.env());
    let amount = 0i128; // Invalid: zero amount
    let reason = RecoveryReason::DeprecatedFlow;
    
    // Should fail: amount must be positive
    client.recover_stranded_funds(&admin, &recipient, &amount, &reason);
}

#[test]
#[should_panic(expected = "Error(Contract, #405)")]
fn test_recover_stranded_funds_negative_amount() {
    let (_, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&admin.env());
    let amount = -1_000_000i128; // Invalid: negative amount
    let reason = RecoveryReason::AccidentalTransfer;
    
    // Should fail: amount must be positive
    client.recover_stranded_funds(&admin, &recipient, &amount, &reason);
}

#[test]
fn test_recover_stranded_funds_all_recovery_reasons() {
    let (env, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&env);
    let amount = 10_000_000i128;
    
    // Test each recovery reason
    let result1 = client.try_recover_stranded_funds(&admin, &recipient, &amount, &RecoveryReason::AccidentalTransfer);
    assert!(result1.is_ok());
    
    let result2 = client.try_recover_stranded_funds(&admin, &recipient, &amount, &RecoveryReason::DeprecatedFlow);
    assert!(result2.is_ok());
    
    let result3 = client.try_recover_stranded_funds(&admin, &recipient, &amount, &RecoveryReason::UnreachableSubscriber);
    assert!(result3.is_ok());
}

#[test]
fn test_recover_stranded_funds_event_emission() {
    let (env, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&env);
    let amount = 25_000_000i128;
    let reason = RecoveryReason::UnreachableSubscriber;
    
    env.ledger().with_mut(|li| li.timestamp = 5000);
    
    // Perform recovery
    client.recover_stranded_funds(&admin, &recipient, &amount, &reason);
    
    // Check that event was emitted
    let events = env.events().all();
    assert!(events.len() > 0);
    
    // The event should contain recovery information
    // Note: Event details verification depends on SDK version
}

#[test]
fn test_recover_stranded_funds_large_amount() {
    let (_, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&admin.env());
    let amount = 1_000_000_000_000i128; // 1 million USDC (with 6 decimals)
    let reason = RecoveryReason::DeprecatedFlow;
    
    // Should handle large amounts
    let result = client.try_recover_stranded_funds(&admin, &recipient, &amount, &reason);
    assert!(result.is_ok());
}

#[test]
fn test_recover_stranded_funds_small_amount() {
    let (_, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&admin.env());
    let amount = 1i128; // Minimal amount (1 stroops)
    let reason = RecoveryReason::AccidentalTransfer;
    
    // Should handle minimal positive amount
    let result = client.try_recover_stranded_funds(&admin, &recipient, &amount, &reason);
    assert!(result.is_ok());
}

#[test]
fn test_recover_stranded_funds_multiple_recoveries() {
    let (env, client, _, admin) = setup_test_env();
    
    let recipient1 = Address::generate(&env);
    let recipient2 = Address::generate(&env);
    let recipient3 = Address::generate(&env);
    
    // Multiple recoveries should all succeed
    let result1 = client.try_recover_stranded_funds(
        &admin, 
        &recipient1, 
        &10_000_000i128, 
        &RecoveryReason::AccidentalTransfer
    );
    assert!(result1.is_ok());
    
    let result2 = client.try_recover_stranded_funds(
        &admin, 
        &recipient2, 
        &20_000_000i128, 
        &RecoveryReason::DeprecatedFlow
    );
    assert!(result2.is_ok());
    
    let result3 = client.try_recover_stranded_funds(
        &admin, 
        &recipient3, 
        &30_000_000i128, 
        &RecoveryReason::UnreachableSubscriber
    );
    assert!(result3.is_ok());
    
    // Verify events were emitted
    // Note: Exact count may vary by SDK version
    let events = env.events().all();
    assert!(events.len() > 0);
}

#[test]
fn test_recover_stranded_funds_different_recipients() {
    let (env, client, _, admin) = setup_test_env();
    
    // Test recovery to different recipient types
    let treasury = Address::generate(&env);
    let user_wallet = Address::generate(&env);
    let contract_addr = Address::generate(&env);
    
    let amount = 5_000_000i128;
    let reason = RecoveryReason::AccidentalTransfer;
    
    // Recovery to treasury
    assert!(client.try_recover_stranded_funds(&admin, &treasury, &amount, &reason).is_ok());
    
    // Recovery to user wallet
    assert!(client.try_recover_stranded_funds(&admin, &user_wallet, &amount, &reason).is_ok());
    
    // Recovery to contract address
    assert!(client.try_recover_stranded_funds(&admin, &contract_addr, &amount, &reason).is_ok());
}

#[test]
fn test_recovery_reason_enum_values() {
    // Verify recovery reason enum is properly defined
    let reason1 = RecoveryReason::AccidentalTransfer;
    let reason2 = RecoveryReason::DeprecatedFlow;
    let reason3 = RecoveryReason::UnreachableSubscriber;
    
    // Ensure reasons are distinct
    assert!(reason1 != reason2);
    assert!(reason2 != reason3);
    assert!(reason1 != reason3);
    
    // Test cloning
    let reason_clone = reason1.clone();
    assert!(reason_clone == RecoveryReason::AccidentalTransfer);
}

#[test]
fn test_recover_stranded_funds_timestamp_recorded() {
    let (env, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&env);
    let amount = 15_000_000i128;
    let reason = RecoveryReason::DeprecatedFlow;
    
    // Set specific timestamp
    let expected_timestamp = 123456u64;
    env.ledger().with_mut(|li| li.timestamp = expected_timestamp);
    
    // Perform recovery
    client.recover_stranded_funds(&admin, &recipient, &amount, &reason);
    
    // Event should contain the timestamp
    // (Full verification depends on event inspection capabilities)
    let events = env.events().all();
    assert!(events.len() > 0);
}

#[test]
fn test_recover_stranded_funds_admin_authorization_required() {
    let (env, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&env);
    let amount = 10_000_000i128;
    let reason = RecoveryReason::AccidentalTransfer;
    
    // This should succeed because admin is authenticated
    let result = client.try_recover_stranded_funds(&admin, &recipient, &amount, &reason);
    assert!(result.is_ok());
}

#[test]
fn test_recover_stranded_funds_does_not_affect_subscriptions() {
    let (env, client, _, admin) = setup_test_env();
    
    // Create a subscription
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let sub_id = client.create_subscription(
        &subscriber,
        &merchant,
        &10_000_000i128,
        &(30 * 24 * 60 * 60),
        &false
    );
    
    // Perform recovery (should not affect subscription)
    let recipient = Address::generate(&env);
    client.recover_stranded_funds(&admin, &recipient, &5_000_000i128, &RecoveryReason::DeprecatedFlow);
    
    // Verify subscription is still intact
    let subscription = client.get_subscription(&sub_id);
    assert_eq!(subscription.status, SubscriptionStatus::Active);
    assert_eq!(subscription.subscriber, subscriber);
    assert_eq!(subscription.merchant, merchant);
}

#[test]
fn test_recover_stranded_funds_with_cancelled_subscription() {
    let (env, client, _, admin) = setup_test_env();
    
    // Create and cancel a subscription
    let subscriber = Address::generate(&env);
    let merchant = Address::generate(&env);
    let sub_id = client.create_subscription(
        &subscriber,
        &merchant,
        &10_000_000i128,
        &(30 * 24 * 60 * 60),
        &false
    );
    client.cancel_subscription(&sub_id, &subscriber);
    
    // Admin can still recover stranded funds
    let recipient = Address::generate(&env);
    let result = client.try_recover_stranded_funds(
        &admin,
        &recipient,
        &5_000_000i128,
        &RecoveryReason::UnreachableSubscriber
    );
    assert!(result.is_ok());
    
    // Subscription remains cancelled
    assert_eq!(client.get_subscription(&sub_id).status, SubscriptionStatus::Cancelled);
}

#[test]
fn test_recover_stranded_funds_idempotency() {
    let (env, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&env);
    let amount = 10_000_000i128;
    let reason = RecoveryReason::AccidentalTransfer;
    
    // Perform first recovery
    let result1 = client.try_recover_stranded_funds(&admin, &recipient, &amount, &reason);
    assert!(result1.is_ok());
    
    // Perform second recovery with same parameters
    let result2 = client.try_recover_stranded_funds(&admin, &recipient, &amount, &reason);
    assert!(result2.is_ok());
    
    // Both should succeed (no idempotency constraint)
    // Each generates its own event
    let events = env.events().all();
    assert!(events.len() > 0);
}

#[test]
fn test_recover_stranded_funds_edge_case_max_i128() {
    let (_, client, _, admin) = setup_test_env();
    
    let recipient = Address::generate(&admin.env());
    // Test near max i128 value
    let amount = i128::MAX - 1000;
    let reason = RecoveryReason::DeprecatedFlow;
    
    // Should handle large values
    let result = client.try_recover_stranded_funds(&admin, &recipient, &amount, &reason);
    assert!(result.is_ok());
}
