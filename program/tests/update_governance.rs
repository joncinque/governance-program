#![cfg(feature = "test-sbf")]

mod setup;

use {
    paladin_governance_program::{
        error::PaladinGovernanceError,
        instruction::update_governance,
        state::{get_governance_address, Config, Proposal},
    },
    setup::{setup, setup_governance, setup_proposal},
    solana_program_test::*,
    solana_sdk::{
        account::AccountSharedData,
        instruction::InstructionError,
        pubkey::Pubkey,
        signer::Signer,
        transaction::{Transaction, TransactionError},
    },
};

#[tokio::test]
async fn fail_incorrect_vault_account() {
    // TODO!
}

#[tokio::test]
async fn fail_governance_incorrect_address() {
    let proposal = Pubkey::new_unique();
    let governance = Pubkey::new_unique(); // Incorrect governance address.

    let mut context = setup().start_with_context().await;

    let instruction = update_governance(
        &governance,
        &proposal,
        /* cooldown_period_seconds */ 0,
        /* proposal_acceptance_threshold */ 0,
        /* proposal_rejection_threshold */ 0,
    );

    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    );

    let err = context
        .banks_client
        .process_transaction(transaction)
        .await
        .unwrap_err()
        .unwrap();

    assert_eq!(
        err,
        TransactionError::InstructionError(
            0,
            InstructionError::Custom(
                PaladinGovernanceError::IncorrectGovernanceConfigAddress as u32
            )
        )
    );
}

#[tokio::test]
async fn fail_governance_incorrect_owner() {
    let proposal = Pubkey::new_unique();
    let governance = get_governance_address(&paladin_governance_program::id());

    let mut context = setup().start_with_context().await;

    // Set up a governance account with an incorrect owner.
    {
        let rent = context.banks_client.get_rent().await.unwrap();
        let space = std::mem::size_of::<Config>();
        let lamports = rent.minimum_balance(space);
        context.set_account(
            &governance,
            &AccountSharedData::new(lamports, space, &Pubkey::new_unique()), // Incorrect owner.
        );
    }

    let instruction = update_governance(
        &governance,
        &proposal,
        /* cooldown_period_seconds */ 0,
        /* proposal_acceptance_threshold */ 0,
        /* proposal_rejection_threshold */ 0,
    );

    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    );

    let err = context
        .banks_client
        .process_transaction(transaction)
        .await
        .unwrap_err()
        .unwrap();

    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::InvalidAccountOwner)
    );
}

#[tokio::test]
async fn fail_governance_not_initialized() {
    let proposal = Pubkey::new_unique();
    let governance = get_governance_address(&paladin_governance_program::id());

    let mut context = setup().start_with_context().await;

    // Set up an uninitialized governance account.
    {
        let rent = context.banks_client.get_rent().await.unwrap();
        let lamports = rent.minimum_balance(0);
        context.set_account(
            &governance,
            &AccountSharedData::new(lamports, 0, &paladin_governance_program::id()),
        );
    }

    let instruction = update_governance(
        &governance,
        &proposal,
        /* cooldown_period_seconds */ 0,
        /* proposal_acceptance_threshold */ 0,
        /* proposal_rejection_threshold */ 0,
    );

    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    );

    let err = context
        .banks_client
        .process_transaction(transaction)
        .await
        .unwrap_err()
        .unwrap();

    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::UninitializedAccount)
    );
}

#[tokio::test]
async fn fail_proposal_incorrect_owner() {
    let proposal = Pubkey::new_unique();
    let governance = get_governance_address(&paladin_governance_program::id());

    let mut context = setup().start_with_context().await;
    setup_governance(&mut context, &governance, 0, 0, 0, 0).await;

    // Set up the proposal account with the incorrect owner.
    {
        let rent = context.banks_client.get_rent().await.unwrap();
        let space = std::mem::size_of::<Proposal>();
        let lamports = rent.minimum_balance(space);
        context.set_account(
            &proposal,
            &AccountSharedData::new(lamports, space, &Pubkey::new_unique()), // Incorrect owner.
        );
    }

    let instruction = update_governance(
        &governance,
        &proposal,
        /* cooldown_period_seconds */ 0,
        /* proposal_acceptance_threshold */ 0,
        /* proposal_rejection_threshold */ 0,
    );

    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    );

    let err = context
        .banks_client
        .process_transaction(transaction)
        .await
        .unwrap_err()
        .unwrap();

    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::InvalidAccountOwner)
    );
}

#[tokio::test]
async fn fail_proposal_not_initialized() {
    let proposal = Pubkey::new_unique();
    let governance = get_governance_address(&paladin_governance_program::id());

    let mut context = setup().start_with_context().await;
    setup_governance(&mut context, &governance, 0, 0, 0, 0).await;

    // Set up the proposal account uninitialized.
    {
        let rent = context.banks_client.get_rent().await.unwrap();
        let space = std::mem::size_of::<Proposal>();
        let lamports = rent.minimum_balance(space);
        context.set_account(
            &proposal,
            &AccountSharedData::new(lamports, space, &paladin_governance_program::id()),
        );
    }

    let instruction = update_governance(
        &governance,
        &proposal,
        /* cooldown_period_seconds */ 0,
        /* proposal_acceptance_threshold */ 0,
        /* proposal_rejection_threshold */ 0,
    );

    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    );

    let err = context
        .banks_client
        .process_transaction(transaction)
        .await
        .unwrap_err()
        .unwrap();

    assert_eq!(
        err,
        TransactionError::InstructionError(0, InstructionError::UninitializedAccount)
    );
}

#[tokio::test]
async fn success() {
    let proposal = Pubkey::new_unique();
    let governance = get_governance_address(&paladin_governance_program::id());

    let mut context = setup().start_with_context().await;
    setup_governance(&mut context, &governance, 0, 0, 0, 0).await;
    setup_proposal(&mut context, &proposal, &Pubkey::new_unique(), 0, 0).await;

    let instruction = update_governance(
        &governance,
        &proposal,
        /* cooldown_period_seconds */ 1,
        /* proposal_acceptance_threshold */ 2,
        /* proposal_rejection_threshold */ 3,
    );

    let transaction = Transaction::new_signed_with_payer(
        &[instruction],
        Some(&context.payer.pubkey()),
        &[&context.payer],
        context.last_blockhash,
    );

    context
        .banks_client
        .process_transaction(transaction)
        .await
        .unwrap();

    // Assert the governance account was updated.
    let governance_account = context
        .banks_client
        .get_account(governance)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bytemuck::from_bytes::<Config>(&governance_account.data),
        &Config {
            cooldown_period_seconds: 1,
            proposal_acceptance_threshold: 2,
            proposal_rejection_threshold: 3,
            total_staked: 0,
        }
    );
}