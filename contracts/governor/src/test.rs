#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, String,
};

fn setup_governor(
    env: &Env,
    voting_delay: u64,
    voting_period: u64,
    proposal_threshold: i128,
    quorum_votes: i128,
) -> (Address, Address, Address, GovernorContractClient<'_>) {

    let admin = Address::generate(env);
    let token = Address::generate(env);
    let contract_id = env.register(GovernorContract, ());
    let client = GovernorContractClient::new(env, &contract_id);

    client.initialize(
        &admin,
        &token,
        &proposal_threshold,
        &voting_delay,
        &voting_period,
        &quorum_votes,
    );

    (contract_id, admin, token, client)
}

#[test]
fn test_initialize_and_config() {
    let env = Env::default();
    let (_, admin, token, client) = setup_governor(&env, 100, 1000, 500, 1000);

    let config = client.get_config();
    assert_eq!(config.admin, admin);
    assert_eq!(config.staking_token, token);
    assert_eq!(config.proposal_threshold, 500);
    assert_eq!(config.voting_delay, 100);
    assert_eq!(config.voting_period, 1000);
    assert_eq!(config.quorum_votes, 1000);

    assert_eq!(client.get_fee_structure(), 30);
    assert_eq!(client.get_amm_pairs().len(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")] // AlreadyInitialized
fn test_double_initialize_rejected() {
    let env = Env::default();
    let (_, admin, token, client) = setup_governor(&env, 100, 1000, 500, 1000);

    client.initialize(&admin, &token, &500, &100, &1000, &1000);
}

#[test]
fn test_staking_and_unstaking() {
    let env = Env::default();
    let user = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 100, 1000, 500, 1000);

    env.mock_all_auths();

    assert_eq!(client.get_staked_balance(&user), 0);
    client.stake(&user, &1000);
    assert_eq!(client.get_staked_balance(&user), 1000);

    client.unstake(&user, &400);
    assert_eq!(client.get_staked_balance(&user), 600);

    client.unstake(&user, &600);
    assert_eq!(client.get_staked_balance(&user), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")] // BelowProposalThreshold
fn test_propose_below_threshold_rejected() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 100, 1000, 500, 1000);

    env.mock_all_auths();
    client.stake(&proposer, &499); // 499 < 500 threshold

    let title = String::from_str(&env, "Lower AMM fee to 10 bps");
    let action = ProposalAction::UpdateFeeStructure(10);
    client.propose(&proposer, &title, &action);
}

#[test]
fn test_proposal_lifecycle_pending_to_active() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 300, 1000, 500, 1000);

    env.mock_all_auths();
    env.ledger().set_timestamp(1000);

    client.stake(&proposer, &500);

    let title = String::from_str(&env, "Lower AMM fee to 20 bps");
    let action = ProposalAction::UpdateFeeStructure(20);
    let prop_id = client.propose(&proposer, &title, &action);
    assert_eq!(prop_id, 1);

    // Timestamp 1000: proposal start_time is 1000 + 300 = 1300 -> state is Pending
    assert_eq!(client.state(&prop_id), ProposalState::Pending);

    // Fast-forward to 1300 (delay passed) -> state is Active
    env.ledger().set_timestamp(1300);
    assert_eq!(client.state(&prop_id), ProposalState::Active);
}

#[test]
fn test_cast_vote_and_double_voting_prevention() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let voter = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 0, 1000, 500, 1000);

    env.mock_all_auths();
    env.ledger().set_timestamp(1000);

    client.stake(&proposer, &1000);
    client.stake(&voter, &2000);

    let title = String::from_str(&env, "Test double voting");
    let action = ProposalAction::UpdateFeeStructure(25);
    let prop_id = client.propose(&proposer, &title, &action);

    assert_eq!(client.has_voted(&prop_id, &voter), false);

    // Cast vote For
    let weight = client.cast_vote(&voter, &prop_id, &VoteType::For);
    assert_eq!(weight, 2000);
    assert_eq!(client.has_voted(&prop_id, &voter), true);

    let prop = client.get_proposal(&prop_id);
    assert_eq!(prop.for_votes, 2000);
    assert_eq!(prop.against_votes, 0);

    // Second vote by same voter on same proposal must be rejected
    let res = client.try_cast_vote(&voter, &prop_id, &VoteType::Against);
    assert_eq!(res, Err(Ok(Error::AlreadyVoted)));
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")] // ZeroVotingPower
fn test_vote_with_zero_power_rejected() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let non_staker = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 0, 1000, 500, 1000);

    env.mock_all_auths();
    env.ledger().set_timestamp(1000);

    client.stake(&proposer, &1000);
    let title = String::from_str(&env, "Zero power test");
    let prop_id = client.propose(&proposer, &title, &ProposalAction::UpdateFeeStructure(15));

    client.cast_vote(&non_staker, &prop_id, &VoteType::For);
}

#[test]
fn test_proposal_succeeded_and_execution() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let voter1 = Address::generate(&env);
    let voter2 = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 0, 1000, 500, 1500);

    env.mock_all_auths();
    env.ledger().set_timestamp(1000);

    client.stake(&proposer, &500);
    client.stake(&voter1, &1600);
    client.stake(&voter2, &400);

    let title = String::from_str(&env, "Update fee to 15 bps");
    let prop_id = client.propose(&proposer, &title, &ProposalAction::UpdateFeeStructure(15));

    // Voters cast votes: 1600 For vs 400 Against
    client.cast_vote(&voter1, &prop_id, &VoteType::For);
    client.cast_vote(&voter2, &prop_id, &VoteType::Against);

    // Before voting period ends, state is Active
    assert_eq!(client.state(&prop_id), ProposalState::Active);

    // Cannot execute while Active
    let try_exec = client.try_execute(&prop_id);
    assert_eq!(try_exec, Err(Ok(Error::ProposalNotSucceeded)));

    // Fast-forward past voting period (1000 + 1000 = 2000)
    env.ledger().set_timestamp(2001);

    // 1600 For > 400 Against and 1600 >= 1500 quorum -> Succeeded!
    assert_eq!(client.state(&prop_id), ProposalState::Succeeded);

    // Execute proposal
    client.execute(&prop_id);

    // State becomes Executed
    assert_eq!(client.state(&prop_id), ProposalState::Executed);

    // Administrative variable updated!
    assert_eq!(client.get_fee_structure(), 15);

    // Double execution fails
    let double_exec = client.try_execute(&prop_id);
    assert_eq!(double_exec, Err(Ok(Error::ProposalNotSucceeded)));
}

#[test]
fn test_proposal_defeated_when_against_dominates() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let voter1 = Address::generate(&env);
    let voter2 = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 0, 1000, 500, 1000);

    env.mock_all_auths();
    env.ledger().set_timestamp(1000);

    client.stake(&proposer, &500);
    client.stake(&voter1, &600);
    client.stake(&voter2, &2000);

    let prop_id = client.propose(
        &proposer,
        &String::from_str(&env, "Bad idea"),
        &ProposalAction::UpdateFeeStructure(99),
    );

    client.cast_vote(&voter1, &prop_id, &VoteType::For);
    client.cast_vote(&voter2, &prop_id, &VoteType::Against);

    env.ledger().set_timestamp(2001);

    // 600 For vs 2000 Against -> Defeated!
    assert_eq!(client.state(&prop_id), ProposalState::Defeated);

    let exec_res = client.try_execute(&prop_id);
    assert_eq!(exec_res, Err(Ok(Error::ProposalNotSucceeded)));
}

#[test]
fn test_proposal_defeated_when_quorum_not_met() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let voter = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 0, 1000, 500, 2000); // quorum = 2000

    env.mock_all_auths();
    env.ledger().set_timestamp(1000);

    client.stake(&proposer, &500);
    client.stake(&voter, &1000);

    let prop_id = client.propose(
        &proposer,
        &String::from_str(&env, "Low turnout"),
        &ProposalAction::UpdateFeeStructure(20),
    );

    client.cast_vote(&voter, &prop_id, &VoteType::For);

    env.ledger().set_timestamp(2001);

    // 1000 For > 0 Against, but 1000 < 2000 quorum -> Defeated!
    assert_eq!(client.state(&prop_id), ProposalState::Defeated);
}

#[test]
fn test_execute_add_amm_pair() {
    let env = Env::default();
    let proposer = Address::generate(&env);
    let voter = Address::generate(&env);
    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let (_, _, _, client) = setup_governor(&env, 0, 500, 100, 500);

    env.mock_all_auths();
    env.ledger().set_timestamp(1000);

    client.stake(&proposer, &200);
    client.stake(&voter, &1000);

    let action = ProposalAction::AddAmmPair((token_a.clone(), token_b.clone()));
    let prop_id = client.propose(
        &proposer,
        &String::from_str(&env, "Add FLOW/USDC AMM pair"),
        &action,
    );

    client.cast_vote(&voter, &prop_id, &VoteType::For);

    env.ledger().set_timestamp(1501);
    assert_eq!(client.state(&prop_id), ProposalState::Succeeded);

    client.execute(&prop_id);

    let pairs = client.get_amm_pairs();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs.get(0).unwrap(), (token_a, token_b));
}
