//! Program processor.

use {
    crate::{
        error::PaladinGovernanceError,
        instruction::PaladinGovernanceInstruction,
        state::{
            collect_governance_signer_seeds, collect_proposal_transaction_signer_seeds,
            collect_proposal_vote_signer_seeds, collect_treasury_signer_seeds,
            get_governance_address_and_bump_seed, get_proposal_transaction_address,
            get_proposal_transaction_address_and_bump_seed, get_proposal_vote_address,
            get_proposal_vote_address_and_bump_seed, get_treasury_address_and_bump_seed, Config,
            Proposal, ProposalAccountMeta, ProposalInstruction, ProposalStatus,
            ProposalTransaction, ProposalVote, ProposalVoteElection,
        },
    },
    borsh::BorshDeserialize,
    paladin_stake_program::state::{find_stake_pda, Config as StakeConfig, Stake},
    solana_program::{
        account_info::{next_account_info, AccountInfo},
        borsh1::get_instance_packed_len,
        clock::Clock,
        entrypoint::ProgramResult,
        instruction::Instruction,
        msg,
        program::invoke_signed,
        program_error::ProgramError,
        pubkey::Pubkey,
        system_instruction,
        sysvar::Sysvar,
    },
    spl_discriminator::{ArrayDiscriminator, SplDiscriminate},
    std::num::NonZeroU64,
};

const THRESHOLD_SCALING_FACTOR: u64 = 1_000_000_000; // 1e9

fn calculate_proposal_vote_threshold(stake: u64, total_stake: u64) -> Result<u32, ProgramError> {
    if total_stake == 0 {
        return Ok(0);
    }
    // Calculation: stake / total_stake
    //
    // Scaled by 1e9 to store 9 decimal places of precision.
    stake
        .checked_mul(THRESHOLD_SCALING_FACTOR)
        .and_then(|product| product.checked_div(total_stake))
        .and_then(|result| u32::try_from(result).ok())
        .ok_or(ProgramError::ArithmeticOverflow)
}

fn get_stake_checked(
    authority_key: &Pubkey,
    stake_config_address: &Pubkey,
    stake_info: &AccountInfo,
) -> Result<u64, ProgramError> {
    check_stake_exists(stake_info)?;

    let data = stake_info.try_borrow_data()?;
    let state =
        bytemuck::try_from_bytes::<Stake>(&data).map_err(|_| ProgramError::InvalidAccountData)?;

    // Ensure the stake account belongs to the authority.
    if state.authority != *authority_key {
        return Err(ProgramError::IncorrectAuthority);
    }

    // Ensure the stake account has the correct address derived from the
    // validator vote account and the stake config account.
    if stake_info.key
        != &find_stake_pda(
            &state.validator_vote,
            stake_config_address,
            &paladin_stake_program::id(),
        )
        .0
    {
        return Err(PaladinGovernanceError::StakeConfigMismatch.into());
    }

    Ok(state.amount)
}

fn check_stake_config_exists(stake_config_info: &AccountInfo) -> ProgramResult {
    // Ensure the stake config account is owned by the Paladin Stake program.
    if stake_config_info.owner != &paladin_stake_program::id() {
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Ensure the stake account is initialized.
    if !(stake_config_info.data_len() == std::mem::size_of::<StakeConfig>()
        && &stake_config_info.try_borrow_data()?[0..8] == StakeConfig::SPL_DISCRIMINATOR_SLICE)
    {
        return Err(ProgramError::UninitializedAccount);
    }

    Ok(())
}

fn check_stake_exists(stake_info: &AccountInfo) -> ProgramResult {
    // Ensure the stake account is owned by the Paladin Stake program.
    if stake_info.owner != &paladin_stake_program::id() {
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Ensure the stake account is initialized.
    if !(stake_info.data_len() == std::mem::size_of::<Stake>()
        && &stake_info.try_borrow_data()?[0..8] == Stake::SPL_DISCRIMINATOR_SLICE)
    {
        return Err(ProgramError::UninitializedAccount);
    }

    Ok(())
}

fn check_governance_exists(program_id: &Pubkey, governance_info: &AccountInfo) -> ProgramResult {
    // Ensure the governance account is owned by the Paladin Governance program.
    if governance_info.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Ensure the governance account is initialized.
    if governance_info.data_len() != std::mem::size_of::<Config>() {
        return Err(ProgramError::UninitializedAccount);
    }

    Ok(())
}

fn check_proposal_exists(program_id: &Pubkey, proposal_info: &AccountInfo) -> ProgramResult {
    // Ensure the proposal account is owned by the Paladin Governance program.
    if proposal_info.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Ensure the proposal account is initialized.
    if !(proposal_info.data_len() == std::mem::size_of::<Proposal>()
        && &proposal_info.try_borrow_data()?[0..8] == Proposal::SPL_DISCRIMINATOR_SLICE)
    {
        return Err(ProgramError::UninitializedAccount);
    }

    Ok(())
}

fn check_proposal_transaction_exists(
    program_id: &Pubkey,
    proposal_transaction_info: &AccountInfo,
) -> ProgramResult {
    // Ensure the proposal transaction account is owned by the Paladin
    // Governance program.
    if proposal_transaction_info.owner != program_id {
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Ensure the proposal transaction account is initialized.
    if proposal_transaction_info.data_len() == 0 {
        return Err(ProgramError::UninitializedAccount);
    }

    Ok(())
}

/// Processes a
/// [CreateProposal](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_create_proposal(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let stake_authority_info = next_account_info(accounts_iter)?;
    let stake_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;
    let proposal_transaction_info = next_account_info(accounts_iter)?;
    let governance_info = next_account_info(accounts_iter)?;

    // Ensure the stake authority is a signer.
    if !stake_authority_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    // Ensure a valid stake account was provided.
    {
        check_stake_exists(stake_info)?;

        let data = stake_info.try_borrow_data()?;
        let state = bytemuck::try_from_bytes::<Stake>(&data)
            .map_err(|_| ProgramError::InvalidAccountData)?;

        // Ensure the stake account belongs to the authority.
        if state.authority != *stake_authority_info.key {
            return Err(ProgramError::IncorrectAuthority);
        }
    }

    check_governance_exists(program_id, governance_info)?;

    let governance_config = {
        let governance_data = governance_info.try_borrow_data()?;
        *bytemuck::try_from_bytes::<Config>(&governance_data)
            .map_err(|_| ProgramError::InvalidAccountData)?
    };

    // Initialize the proposal account.
    {
        // Ensure the proposal account is owned by the Paladin Governance program.
        if proposal_info.owner != program_id {
            return Err(ProgramError::InvalidAccountOwner);
        }

        // Ensure the proposal account has enough space.
        if proposal_info.data_len() != std::mem::size_of::<Proposal>() {
            return Err(ProgramError::InvalidAccountData);
        }

        // Ensure the proposal account is not initialized.
        if &proposal_info.try_borrow_data()?[0..8] != ArrayDiscriminator::UNINITIALIZED.as_slice() {
            return Err(ProgramError::AccountAlreadyInitialized);
        }

        let clock = <Clock as Sysvar>::get()?;
        let creation_timestamp = clock.unix_timestamp;

        // Write the data.
        let mut proposal_data = proposal_info.try_borrow_mut_data()?;
        *bytemuck::try_from_bytes_mut::<Proposal>(&mut proposal_data)
            .map_err(|_| ProgramError::InvalidAccountData)? = Proposal::new(
            stake_authority_info.key,
            creation_timestamp,
            governance_config,
        );
    }

    // Initialize the proposal transaction account.
    {
        let (proposal_transaction_address, signer_bump_seed) =
            get_proposal_transaction_address_and_bump_seed(proposal_info.key, program_id);
        let bump_seed = [signer_bump_seed];
        let proposal_transaction_signer_seeds =
            collect_proposal_transaction_signer_seeds(proposal_info.key, &bump_seed);

        // Ensure the provided proposal transaction address is the correct
        // address derived from the program.
        if !proposal_transaction_info
            .key
            .eq(&proposal_transaction_address)
        {
            return Err(PaladinGovernanceError::IncorrectProposalTransactionAddress.into());
        }

        // Ensure the proposal transaction account has not already been
        // initialized.
        if proposal_transaction_info.data_len() != 0 {
            return Err(ProgramError::AccountAlreadyInitialized);
        }

        let state = ProposalTransaction::default();
        let space = get_instance_packed_len(&state)? as u64;

        // Allocate & assign.
        invoke_signed(
            &system_instruction::allocate(&proposal_transaction_address, space),
            &[proposal_transaction_info.clone()],
            &[&proposal_transaction_signer_seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(&proposal_transaction_address, program_id),
            &[proposal_transaction_info.clone()],
            &[&proposal_transaction_signer_seeds],
        )?;

        // Write the data.
        borsh::to_writer(&mut proposal_transaction_info.data.borrow_mut()[..], &state)?;
    }

    Ok(())
}

/// Processes a
/// [PushInstruction](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_push_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_program_id: Pubkey,
    instruction_account_metas: Vec<ProposalAccountMeta>,
    instruction_data: Vec<u8>,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let stake_authority_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;
    let proposal_transaction_info = next_account_info(accounts_iter)?;

    // Ensure the stake authority is a signer.
    if !stake_authority_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    check_proposal_exists(program_id, proposal_info)?;

    let proposal_data = proposal_info.try_borrow_data()?;
    let proposal_state = bytemuck::try_from_bytes::<Proposal>(&proposal_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    // Ensure the stake authority is the proposal author.
    proposal_state.check_author(stake_authority_info.key)?;

    // Ensure the proposal is in draft stage.
    if proposal_state.status != ProposalStatus::Draft {
        return Err(PaladinGovernanceError::ProposalIsImmutable.into());
    }

    // Ensure the provided proposal transaction address is the correct address
    // derived from the proposal.
    if !proposal_transaction_info
        .key
        .eq(&get_proposal_transaction_address(
            proposal_info.key,
            program_id,
        ))
    {
        return Err(PaladinGovernanceError::IncorrectProposalTransactionAddress.into());
    }

    check_proposal_transaction_exists(program_id, proposal_transaction_info)?;

    let mut proposal_transaction_state =
        ProposalTransaction::try_from_slice(&proposal_transaction_info.try_borrow_data()?)?;

    // Insert the instruction.
    let new_instruction = ProposalInstruction::new(
        &instruction_program_id,
        instruction_account_metas,
        instruction_data,
    );
    proposal_transaction_state
        .instructions
        .push(new_instruction);

    // Reallocate the account.
    let new_len = get_instance_packed_len(&proposal_transaction_state)?;
    proposal_transaction_info.realloc(new_len, true)?;

    // Write the data.
    borsh::to_writer(
        &mut proposal_transaction_info.data.borrow_mut()[..],
        &proposal_transaction_state,
    )?;

    Ok(())
}

/// Processes a
/// [RemoveInstruction](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_remove_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_index: u32,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let stake_authority_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;
    let proposal_transaction_info = next_account_info(accounts_iter)?;

    // Ensure the stake authority is a signer.
    if !stake_authority_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    check_proposal_exists(program_id, proposal_info)?;

    let proposal_data = proposal_info.try_borrow_data()?;
    let proposal_state = bytemuck::try_from_bytes::<Proposal>(&proposal_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    // Ensure the stake authority is the proposal author.
    proposal_state.check_author(stake_authority_info.key)?;

    // Ensure the proposal is in draft stage.
    if proposal_state.status != ProposalStatus::Draft {
        return Err(PaladinGovernanceError::ProposalIsImmutable.into());
    }

    // Ensure the provided proposal transaction account has the correct address
    // derived from the proposal.
    if !proposal_transaction_info
        .key
        .eq(&get_proposal_transaction_address(
            proposal_info.key,
            program_id,
        ))
    {
        return Err(PaladinGovernanceError::IncorrectProposalTransactionAddress.into());
    }

    check_proposal_transaction_exists(program_id, proposal_transaction_info)?;

    let mut proposal_transaction_state =
        ProposalTransaction::try_from_slice(&proposal_transaction_info.try_borrow_data()?)?;

    // Ensure the index is valid.
    let instruction_index = instruction_index as usize;
    if instruction_index >= proposal_transaction_state.instructions.len() {
        return Err(PaladinGovernanceError::InvalidTransactionIndex.into());
    }

    // Remove the instruction.
    proposal_transaction_state
        .instructions
        .remove(instruction_index);

    // Reallocate the account.
    let new_len = get_instance_packed_len(&proposal_transaction_state)?;
    proposal_transaction_info.realloc(new_len, true)?;

    // Write the data.
    borsh::to_writer(
        &mut proposal_transaction_info.data.borrow_mut()[..],
        &proposal_transaction_state,
    )?;

    Ok(())
}

/// Processes a
/// [CancelProposal](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_cancel_proposal(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let stake_authority_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;

    // Ensure the stake authority is a signer.
    if !stake_authority_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    check_proposal_exists(program_id, proposal_info)?;

    let mut proposal_data = proposal_info.try_borrow_mut_data()?;
    let proposal_state = bytemuck::try_from_bytes_mut::<Proposal>(&mut proposal_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    // Ensure the stake authority is the proposal author.
    proposal_state.check_author(stake_authority_info.key)?;

    // Ensure the proposal is in draft or voting stage.
    match proposal_state.status {
        ProposalStatus::Draft | ProposalStatus::Voting => (),
        ProposalStatus::Cancelled
        | ProposalStatus::Accepted
        | ProposalStatus::Rejected
        | ProposalStatus::Processed => {
            return Err(PaladinGovernanceError::ProposalIsImmutable.into())
        }
    }

    // Set the proposal's status to cancelled.
    proposal_state.status = ProposalStatus::Cancelled;

    Ok(())
}

/// Processes a
/// [BeginVoting](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_begin_voting(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let stake_authority_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;

    // Ensure the stake authority is a signer.
    if !stake_authority_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    check_proposal_exists(program_id, proposal_info)?;

    let mut proposal_data = proposal_info.try_borrow_mut_data()?;
    let proposal_state = bytemuck::try_from_bytes_mut::<Proposal>(&mut proposal_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    // Ensure the stake authority is the proposal author.
    proposal_state.check_author(stake_authority_info.key)?;

    // Ensure the proposal is in draft stage.
    if proposal_state.status != ProposalStatus::Draft {
        return Err(PaladinGovernanceError::ProposalIsImmutable.into());
    }

    // Set the proposal's status to voting.
    proposal_state.status = ProposalStatus::Voting;

    // Set the proposal's voting start timestamp.
    let clock = <Clock as Sysvar>::get()?;
    proposal_state.voting_start_timestamp = NonZeroU64::new(clock.unix_timestamp as u64);

    Ok(())
}

/// Processes a
/// [Vote](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_vote(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    election: ProposalVoteElection,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let stake_authority_info = next_account_info(accounts_iter)?;
    let stake_info = next_account_info(accounts_iter)?;
    let stake_config_info = next_account_info(accounts_iter)?;
    let proposal_vote_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;
    let _system_program_info = next_account_info(accounts_iter)?;

    // Ensure the stake authority is a signer.
    if !stake_authority_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let stake = get_stake_checked(stake_authority_info.key, stake_config_info.key, stake_info)?;

    check_stake_config_exists(stake_config_info)?;
    let total_stake =
        bytemuck::try_from_bytes::<StakeConfig>(&stake_config_info.try_borrow_data()?)
            .map_err(|_| ProgramError::InvalidAccountData)?
            .token_amount_delegated;

    check_proposal_exists(program_id, proposal_info)?;

    let mut proposal_data = proposal_info.try_borrow_mut_data()?;
    let proposal_state = bytemuck::try_from_bytes_mut::<Proposal>(&mut proposal_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    let governance_config = proposal_state.governance_config;

    // Ensure the address of the provided stake config account matches the one
    // stored in the proposal's governance config.
    governance_config.check_stake_config(stake_config_info.key)?;

    // Ensure the proposal is in the voting stage.
    if proposal_state.status != ProposalStatus::Voting {
        return Err(PaladinGovernanceError::ProposalNotInVotingStage.into());
    }

    let clock = <Clock as Sysvar>::get()?;

    // If the proposal has an active cooldown period, ensure it has not ended.
    if proposal_state.cooldown_has_ended(&clock) {
        // If the cooldown period has ended, the proposal is accepted.
        proposal_state.status = ProposalStatus::Accepted;
        return Ok(());
    }

    // Cooldown periods take precedence over voting periods. For example, if a
    // voting period expires, but a cooldown period still has time remaining,
    // the proposal will remain open for voting until the cooldown period ends.
    // Cooldown periods end only in an accepted or rejected proposal.
    if proposal_state.cooldown_timestamp.is_none() && proposal_state.voting_has_ended(&clock) {
        proposal_state.status = ProposalStatus::Rejected;
        return Ok(());
    }

    // Create the proposal vote account.
    {
        let (proposal_vote_address, bump_seed) =
            get_proposal_vote_address_and_bump_seed(stake_info.key, proposal_info.key, program_id);
        let bump_seed = [bump_seed];
        let proposal_vote_signer_seeds =
            collect_proposal_vote_signer_seeds(stake_info.key, proposal_info.key, &bump_seed);

        // Ensure the provided proposal vote address is the correct address
        // derived from the stake authority and proposal.
        if !proposal_vote_info.key.eq(&proposal_vote_address) {
            return Err(PaladinGovernanceError::IncorrectProposalVoteAddress.into());
        }

        // Ensure the proposal vote account has not already been initialized.
        if proposal_vote_info.data_len() != 0 {
            return Err(ProgramError::AccountAlreadyInitialized);
        }

        // Allocate & assign.
        invoke_signed(
            &system_instruction::allocate(
                &proposal_vote_address,
                std::mem::size_of::<ProposalVote>() as u64,
            ),
            &[proposal_vote_info.clone()],
            &[&proposal_vote_signer_seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(&proposal_vote_address, program_id),
            &[proposal_vote_info.clone()],
            &[&proposal_vote_signer_seeds],
        )?;

        // Write the data.
        let mut data = proposal_vote_info.try_borrow_mut_data()?;
        *bytemuck::try_from_bytes_mut(&mut data).map_err(|_| ProgramError::InvalidAccountData)? =
            ProposalVote::new(proposal_info.key, stake, stake_authority_info.key, election);
    }

    match election {
        ProposalVoteElection::For => {
            // The vote was in favor. Increase the stake for the proposal.
            proposal_state.stake_for = proposal_state
                .stake_for
                .checked_add(stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            if calculate_proposal_vote_threshold(proposal_state.stake_for, total_stake)?
                >= governance_config.proposal_acceptance_threshold
                && proposal_state.cooldown_timestamp.is_none()
            {
                // If the proposal has met the acceptance threshold, and it's
                // currently not in a cooldown period, begin a new cooldown period.
                proposal_state.cooldown_timestamp = NonZeroU64::new(clock.unix_timestamp as u64);
            }
        }
        ProposalVoteElection::Against => {
            // The vote was against. Increase the stake against the proposal.
            proposal_state.stake_against = proposal_state
                .stake_against
                .checked_add(stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            if calculate_proposal_vote_threshold(proposal_state.stake_against, total_stake)?
                >= governance_config.proposal_rejection_threshold
            {
                // If the proposal has met the rejection threshold, reject the proposal.
                // This is done regardless of any cooldown period.
                proposal_state.status = ProposalStatus::Rejected;
            }
        }
        ProposalVoteElection::DidNotVote => {
            // None-vote. Increase the abstained stake.
            proposal_state.stake_abstained = proposal_state
                .stake_abstained
                .checked_add(stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    Ok(())
}

/// Processes a
/// [SwitchVote](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_switch_vote(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    new_election: ProposalVoteElection,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let stake_authority_info = next_account_info(accounts_iter)?;
    let stake_info = next_account_info(accounts_iter)?;
    let stake_config_info = next_account_info(accounts_iter)?;
    let proposal_vote_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;

    // Ensure the stake authority is a signer.
    if !stake_authority_info.is_signer {
        return Err(ProgramError::MissingRequiredSignature);
    }

    let stake = get_stake_checked(stake_authority_info.key, stake_config_info.key, stake_info)?;

    check_stake_config_exists(stake_config_info)?;
    let total_stake =
        bytemuck::try_from_bytes::<StakeConfig>(&stake_config_info.try_borrow_data()?)
            .map_err(|_| ProgramError::InvalidAccountData)?
            .token_amount_delegated;

    check_proposal_exists(program_id, proposal_info)?;

    let mut proposal_data = proposal_info.try_borrow_mut_data()?;
    let proposal_state = bytemuck::try_from_bytes_mut::<Proposal>(&mut proposal_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    let governance_config = proposal_state.governance_config;

    // Ensure the address of the provided stake config account matches the one
    // stored in the proposal's governance config.
    governance_config.check_stake_config(stake_config_info.key)?;

    // Ensure the proposal is in the voting stage.
    if proposal_state.status != ProposalStatus::Voting {
        return Err(PaladinGovernanceError::ProposalNotInVotingStage.into());
    }

    let clock = <Clock as Sysvar>::get()?;

    // If the proposal has an active cooldown period, ensure it has not ended.
    if proposal_state.cooldown_has_ended(&clock) {
        // If the cooldown period has ended, the proposal is accepted.
        proposal_state.status = ProposalStatus::Accepted;
        return Ok(());
    }

    // Cooldown periods take precedence over voting periods. For example, if a
    // voting period expires, but a cooldown period still has time remaining,
    // the proposal will remain open for voting until the cooldown period ends.
    // Cooldown periods end only in an accepted or rejected proposal.
    if proposal_state.cooldown_timestamp.is_none() && proposal_state.voting_has_ended(&clock) {
        proposal_state.status = ProposalStatus::Rejected;
        return Ok(());
    }

    // Update the proposal vote account.
    let (last_election, last_stake) = {
        // Ensure the provided proposal vote address is the correct address
        // derived from the stake authority and proposal.
        if !proposal_vote_info.key.eq(&get_proposal_vote_address(
            stake_info.key,
            proposal_info.key,
            program_id,
        )) {
            return Err(PaladinGovernanceError::IncorrectProposalVoteAddress.into());
        }

        // Ensure the proposal vote account is owned by the Paladin Governance
        // program.
        if proposal_vote_info.owner != program_id {
            return Err(ProgramError::InvalidAccountOwner);
        }

        // Ensure the proposal vote account is initialized.
        if proposal_vote_info.data_len() != std::mem::size_of::<ProposalVote>() {
            return Err(ProgramError::UninitializedAccount);
        }

        // Update the vote.
        let mut data = proposal_vote_info.try_borrow_mut_data()?;
        let state = bytemuck::try_from_bytes_mut::<ProposalVote>(&mut data)
            .map_err(|_| ProgramError::InvalidAccountData)?;

        (
            std::mem::replace(&mut state.election, new_election),
            std::mem::replace(&mut state.stake, stake),
        )
    };

    // If the program hasn't terminated by this point, the vote has changed.
    // Simply update the proposal by inversing the vote stake.
    match last_election {
        ProposalVoteElection::For => {
            // Previous vote was in favor. Deduct stake for.
            proposal_state.stake_for = proposal_state
                .stake_for
                .checked_sub(last_stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
        ProposalVoteElection::Against => {
            // Previous vote was against. Deduct stake against.
            proposal_state.stake_against = proposal_state
                .stake_against
                .checked_sub(last_stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
        ProposalVoteElection::DidNotVote => {
            // Last vote was a "did not vote". Deduct stake abstained.
            proposal_state.stake_abstained = proposal_state
                .stake_abstained
                .checked_sub(last_stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    match new_election {
        ProposalVoteElection::For => {
            // New vote is in favor. Increment stake for.
            proposal_state.stake_for = proposal_state
                .stake_for
                .checked_add(stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            if calculate_proposal_vote_threshold(proposal_state.stake_for, total_stake)?
                >= governance_config.proposal_acceptance_threshold
                && proposal_state.cooldown_timestamp.is_none()
            {
                // If the proposal has met the acceptance threshold, and it's
                // currently not in a cooldown period, begin a new cooldown period.
                proposal_state.cooldown_timestamp = NonZeroU64::new(clock.unix_timestamp as u64);
            }
        }
        ProposalVoteElection::Against => {
            // New vote is against. Increment stake against.
            proposal_state.stake_against = proposal_state
                .stake_against
                .checked_add(stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;

            if calculate_proposal_vote_threshold(proposal_state.stake_against, total_stake)?
                >= governance_config.proposal_rejection_threshold
            {
                // If the proposal has met the rejection threshold, reject the proposal.
                // This is done regardless of any cooldown period.
                proposal_state.status = ProposalStatus::Rejected;
            }
        }
        ProposalVoteElection::DidNotVote => {
            // New vote is "did not vote". Increment stake abstained.
            proposal_state.stake_abstained = proposal_state
                .stake_abstained
                .checked_add(stake)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    Ok(())
}

/// Processes a
/// [ProcessInstruction](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_index: u32,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let proposal_info = next_account_info(accounts_iter)?;
    let proposal_transaction_info = next_account_info(accounts_iter)?;

    check_proposal_exists(program_id, proposal_info)?;

    let mut proposal_data = proposal_info.try_borrow_mut_data()?;
    let proposal_state = bytemuck::try_from_bytes_mut::<Proposal>(&mut proposal_data)
        .map_err(|_| ProgramError::InvalidAccountData)?;

    // Ensure the proposal was accepted.
    if proposal_state.status != ProposalStatus::Accepted {
        return Err(PaladinGovernanceError::ProposalNotAccepted.into());
    }

    // Ensure the provided proposal transaction account has the correct address
    // derived from the proposal.
    if !proposal_transaction_info
        .key
        .eq(&get_proposal_transaction_address(
            proposal_info.key,
            program_id,
        ))
    {
        return Err(PaladinGovernanceError::IncorrectProposalTransactionAddress.into());
    }

    check_proposal_transaction_exists(program_id, proposal_transaction_info)?;

    let mut proposal_transaction_state =
        ProposalTransaction::try_from_slice(&proposal_transaction_info.try_borrow_data()?)?;

    // Ensure the index is valid.
    let instruction_index = instruction_index as usize;
    if instruction_index >= proposal_transaction_state.instructions.len() {
        return Err(PaladinGovernanceError::InvalidTransactionIndex.into());
    }

    let instruction = &proposal_transaction_state.instructions[instruction_index];

    // Ensure the instruction has not already been executed.
    if instruction.executed {
        return Err(PaladinGovernanceError::InstructionAlreadyExecuted.into());
    }

    // Ensure the previous instruction has been executed.
    if instruction_index > 0
        && !proposal_transaction_state.instructions[instruction_index.saturating_sub(1)].executed
    {
        return Err(PaladinGovernanceError::PreviousInstructionHasNotBeenExecuted.into());
    }

    // Execute the instruction.
    {
        let (_treasury_address, signer_bump_seed) = get_treasury_address_and_bump_seed(
            &proposal_state.governance_config.stake_config_address,
            program_id,
        );
        let bump_seed = [signer_bump_seed];
        let treasury_signer_seeds = collect_treasury_signer_seeds(
            &proposal_state.governance_config.stake_config_address,
            &bump_seed,
        );

        invoke_signed(
            &Instruction::from(instruction),
            accounts_iter.as_slice(),
            &[&treasury_signer_seeds],
        )?;
    }

    // Mark the instruction as executed.
    proposal_transaction_state.instructions[instruction_index].executed = true;

    // Write the data (no reallocation necessary).
    borsh::to_writer(
        &mut proposal_transaction_info.data.borrow_mut()[..],
        &proposal_transaction_state,
    )?;

    Ok(())
}

/// Processes a
/// [InitializeGovernance](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_initialize_governance(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    cooldown_period_seconds: u64,
    proposal_acceptance_threshold: u32,
    proposal_rejection_threshold: u32,
    voting_period_seconds: u64,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let governance_info = next_account_info(accounts_iter)?;
    let stake_config_info = next_account_info(accounts_iter)?;
    let _system_program_info = next_account_info(accounts_iter)?;

    check_stake_config_exists(stake_config_info)?;

    // Create the governance config account.
    {
        let (governance_address, signer_bump_seed) =
            get_governance_address_and_bump_seed(stake_config_info.key, program_id);
        let bump_seed = [signer_bump_seed];
        let governance_signer_seeds =
            collect_governance_signer_seeds(stake_config_info.key, &bump_seed);

        // Ensure the provided governance address is the correct address
        // derived from the program.
        if !governance_info.key.eq(&governance_address) {
            return Err(PaladinGovernanceError::IncorrectGovernanceConfigAddress.into());
        }

        // Ensure the governance account has not already been initialized.
        if governance_info.data_len() != 0 {
            return Err(ProgramError::AccountAlreadyInitialized);
        }

        // Allocate & assign.
        invoke_signed(
            &system_instruction::allocate(
                &governance_address,
                std::mem::size_of::<Config>() as u64,
            ),
            &[governance_info.clone()],
            &[&governance_signer_seeds],
        )?;
        invoke_signed(
            &system_instruction::assign(&governance_address, program_id),
            &[governance_info.clone()],
            &[&governance_signer_seeds],
        )?;

        // Write the data.
        let mut data = governance_info.try_borrow_mut_data()?;
        *bytemuck::try_from_bytes_mut(&mut data).map_err(|_| ProgramError::InvalidAccountData)? =
            Config::new(
                cooldown_period_seconds,
                proposal_acceptance_threshold,
                proposal_rejection_threshold,
                signer_bump_seed,
                stake_config_info.key,
                voting_period_seconds,
            );
    }

    Ok(())
}

/// Processes a
/// [UpdateGovernance](enum.PaladinGovernanceInstruction.html)
/// instruction.
fn process_update_governance(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    cooldown_period_seconds: u64,
    proposal_acceptance_threshold: u32,
    proposal_rejection_threshold: u32,
    voting_period_seconds: u64,
) -> ProgramResult {
    let accounts_iter = &mut accounts.iter();

    let governance_info = next_account_info(accounts_iter)?;
    let proposal_info = next_account_info(accounts_iter)?;

    check_governance_exists(program_id, governance_info)?;
    check_proposal_exists(program_id, proposal_info)?;

    {
        // Ensure the proposal meets the acceptance threshold.
        let proposal_data = proposal_info.try_borrow_data()?;
        let proposal_state = bytemuck::try_from_bytes::<Proposal>(&proposal_data)
            .map_err(|_| ProgramError::InvalidAccountData)?;

        let clock = <Clock as Sysvar>::get()?;

        if !proposal_state.cooldown_has_ended(&clock) {
            return Err(PaladinGovernanceError::ProposalNotAccepted.into());
        }

        // TODO: This instruction requires a gate to ensure it can only be
        // invoked from a proposal.
    }

    // Update the governance config.
    let mut data = governance_info.try_borrow_mut_data()?;
    let state = bytemuck::try_from_bytes_mut::<Config>(&mut data)
        .map_err(|_| ProgramError::InvalidAccountData)?;
    state.cooldown_period_seconds = cooldown_period_seconds;
    state.proposal_acceptance_threshold = proposal_acceptance_threshold;
    state.proposal_rejection_threshold = proposal_rejection_threshold;
    state.voting_period_seconds = voting_period_seconds;

    Ok(())
}

/// Processes a
/// [PaladinGovernanceInstruction](enum.PaladinGovernanceInstruction.html).
pub fn process(program_id: &Pubkey, accounts: &[AccountInfo], input: &[u8]) -> ProgramResult {
    let instruction = PaladinGovernanceInstruction::unpack(input)?;
    match instruction {
        PaladinGovernanceInstruction::CreateProposal => {
            msg!("Instruction: CreateProposal");
            process_create_proposal(program_id, accounts)
        }
        PaladinGovernanceInstruction::PushInstruction {
            instruction_program_id,
            instruction_account_metas,
            instruction_data,
        } => {
            msg!("Instruction: PushInstruction");
            process_push_instruction(
                program_id,
                accounts,
                instruction_program_id,
                instruction_account_metas,
                instruction_data,
            )
        }
        PaladinGovernanceInstruction::RemoveInstruction { instruction_index } => {
            msg!("Instruction: RemoveInstruction");
            process_remove_instruction(program_id, accounts, instruction_index)
        }
        PaladinGovernanceInstruction::CancelProposal => {
            msg!("Instruction: CancelProposal");
            process_cancel_proposal(program_id, accounts)
        }
        PaladinGovernanceInstruction::BeginVoting => {
            msg!("Instruction: BeginVoting");
            process_begin_voting(program_id, accounts)
        }
        PaladinGovernanceInstruction::Vote { election } => {
            msg!("Instruction: Vote");
            process_vote(program_id, accounts, election)
        }
        PaladinGovernanceInstruction::SwitchVote { new_election } => {
            msg!("Instruction: SwitchVote");
            process_switch_vote(program_id, accounts, new_election)
        }
        PaladinGovernanceInstruction::ProcessInstruction { instruction_index } => {
            msg!("Instruction: ProcessInstruction");
            process_process_instruction(program_id, accounts, instruction_index)
        }
        PaladinGovernanceInstruction::InitializeGovernance {
            cooldown_period_seconds,
            proposal_acceptance_threshold,
            proposal_rejection_threshold,
            voting_period_seconds,
        } => {
            msg!("Instruction: InitializeGovernance");
            process_initialize_governance(
                program_id,
                accounts,
                cooldown_period_seconds,
                proposal_acceptance_threshold,
                proposal_rejection_threshold,
                voting_period_seconds,
            )
        }
        PaladinGovernanceInstruction::UpdateGovernance {
            cooldown_period_seconds,
            proposal_acceptance_threshold,
            proposal_rejection_threshold,
            voting_period_seconds,
        } => {
            msg!("Instruction: UpdateGovernance");
            process_update_governance(
                program_id,
                accounts,
                cooldown_period_seconds,
                proposal_acceptance_threshold,
                proposal_rejection_threshold,
                voting_period_seconds,
            )
        }
    }
}
