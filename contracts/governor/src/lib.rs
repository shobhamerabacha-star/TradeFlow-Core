#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    Address, Env, String, Vec,
};


#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InvalidAmount = 4,
    BelowProposalThreshold = 5,
    ProposalNotFound = 6,
    ProposalNotActive = 7,
    AlreadyVoted = 8,
    ZeroVotingPower = 9,
    ProposalNotSucceeded = 10,
    ProposalAlreadyExecuted = 11,
    InvalidVotingPeriod = 12,
    Overflow = 13,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ProposalState {
    Pending = 0,
    Active = 1,
    Defeated = 2,
    Succeeded = 3,
    Executed = 4,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum VoteType {
    Against = 0,
    For = 1,
    Abstain = 2,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProposalAction {
    UpdateFeeStructure(u32),
    AddAmmPair((Address, Address)),
    UpdateProposalThreshold(i128),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GovernorConfig {
    pub admin: Address,
    pub staking_token: Address,
    pub proposal_threshold: i128,
    pub voting_delay: u64,
    pub voting_period: u64,
    pub quorum_votes: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub id: u64,
    pub proposer: Address,
    pub title: String,
    pub action: ProposalAction,
    pub start_time: u64,
    pub end_time: u64,
    pub for_votes: i128,
    pub against_votes: i128,
    pub abstain_votes: i128,
    pub executed: bool,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Config,
    ProposalCount,
    Proposal(u64),
    HasVoted(u64, Address),
    UserVote(u64, Address),
    StakedBalance(Address),
    FeeStructureBps,
    AmmPairs,
}

const DAY_IN_LEDGERS: u32 = 17280;
const BUMP_AMOUNT: u32 = 30 * DAY_IN_LEDGERS;
const LIFETIME_THRESHOLD: u32 = BUMP_AMOUNT - DAY_IN_LEDGERS;

fn bump_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(LIFETIME_THRESHOLD, BUMP_AMOUNT);
}

fn bump_persistent<K: soroban_sdk::IntoVal<Env, soroban_sdk::Val>>(env: &Env, key: &K) {
    env.storage()
        .persistent()
        .extend_ttl(key, LIFETIME_THRESHOLD, BUMP_AMOUNT);
}

#[contract]
pub struct GovernorContract;

#[contractimpl]
impl GovernorContract {
    /// Initialize the Governor contract with governance configuration.
    pub fn initialize(
        env: Env,
        admin: Address,
        staking_token: Address,
        proposal_threshold: i128,
        voting_delay: u64,
        voting_period: u64,
        quorum_votes: i128,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Config) {
            return Err(Error::AlreadyInitialized);
        }

        if voting_period == 0 {
            return Err(Error::InvalidVotingPeriod);
        }
        if proposal_threshold <= 0 || quorum_votes <= 0 {
            return Err(Error::InvalidAmount);
        }

        let config = GovernorConfig {
            admin,
            staking_token,
            proposal_threshold,
            voting_delay,
            voting_period,
            quorum_votes,
        };

        env.storage().instance().set(&DataKey::Config, &config);
        env.storage().instance().set(&DataKey::ProposalCount, &0u64);
        env.storage().instance().set(&DataKey::FeeStructureBps, &30u32); // Default 30 bps (0.3%)
        let pairs: Vec<(Address, Address)> = Vec::new(&env);
        env.storage().instance().set(&DataKey::AmmPairs, &pairs);

        bump_instance(&env);
        Ok(())
    }

    /// Read governor configuration.
    pub fn get_config(env: Env) -> Result<GovernorConfig, Error> {
        bump_instance(&env);
        env.storage()
            .instance()
            .get(&DataKey::Config)
            .ok_or(Error::NotInitialized)
    }

    /// Stake governance tokens to earn voting power.
    pub fn stake(env: Env, user: Address, amount: i128) -> Result<i128, Error> {
        user.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let key = DataKey::StakedBalance(user.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let new_balance = current.checked_add(amount).ok_or(Error::Overflow)?;

        env.storage().persistent().set(&key, &new_balance);
        bump_persistent(&env, &key);
        bump_instance(&env);

        Ok(new_balance)
    }

    /// Unstake governance tokens.
    pub fn unstake(env: Env, user: Address, amount: i128) -> Result<i128, Error> {
        user.require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let key = DataKey::StakedBalance(user.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if current < amount {
            return Err(Error::InvalidAmount);
        }

        let new_balance = current - amount;
        if new_balance == 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &new_balance);
            bump_persistent(&env, &key);
        }

        bump_instance(&env);
        Ok(new_balance)
    }

    /// Retrieve staked balance (voting weight) of an address.
    pub fn get_staked_balance(env: Env, user: Address) -> i128 {
        let key = DataKey::StakedBalance(user);
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if balance > 0 {
            bump_persistent(&env, &key);
        }
        balance
    }

    /// Submit a new governance proposal.
    ///
    /// Requires caller authentication and staked balance >= proposal_threshold.
    pub fn propose(
        env: Env,
        proposer: Address,
        title: String,
        action: ProposalAction,
    ) -> Result<u64, Error> {
        proposer.require_auth();
        bump_instance(&env);

        let config = Self::get_config(env.clone())?;
        let user_staked = Self::get_staked_balance(env.clone(), proposer.clone());

        if user_staked < config.proposal_threshold {
            return Err(Error::BelowProposalThreshold);
        }

        let mut count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ProposalCount)
            .unwrap_or(0);
        count = count.checked_add(1).ok_or(Error::Overflow)?;

        let now = env.ledger().timestamp();
        let start_time = now.checked_add(config.voting_delay).ok_or(Error::Overflow)?;
        let end_time = start_time
            .checked_add(config.voting_period)
            .ok_or(Error::Overflow)?;

        let proposal = Proposal {
            id: count,
            proposer: proposer.clone(),
            title,
            action,
            start_time,
            end_time,
            for_votes: 0,
            against_votes: 0,
            abstain_votes: 0,
            executed: false,
        };

        let prop_key = DataKey::Proposal(count);
        env.storage().persistent().set(&prop_key, &proposal);
        bump_persistent(&env, &prop_key);

        env.storage().instance().set(&DataKey::ProposalCount, &count);

        Ok(count)
    }

    /// Read proposal details by ID.
    pub fn get_proposal(env: Env, proposal_id: u64) -> Result<Proposal, Error> {
        let key = DataKey::Proposal(proposal_id);
        let prop = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::ProposalNotFound)?;
        bump_persistent(&env, &key);
        Ok(prop)
    }

    /// Check current lifecycle state of a proposal.
    pub fn state(env: Env, proposal_id: u64) -> Result<ProposalState, Error> {
        let proposal = Self::get_proposal(env.clone(), proposal_id)?;
        if proposal.executed {
            return Ok(ProposalState::Executed);
        }

        let now = env.ledger().timestamp();
        if now < proposal.start_time {
            return Ok(ProposalState::Pending);
        }

        if now <= proposal.end_time {
            return Ok(ProposalState::Active);
        }

        // Voting has ended; tally votes
        let config = Self::get_config(env.clone())?;
        if proposal.for_votes > proposal.against_votes && proposal.for_votes >= config.quorum_votes {
            Ok(ProposalState::Succeeded)
        } else {
            Ok(ProposalState::Defeated)
        }
    }

    /// Check if a voter has already cast a vote on a proposal.
    pub fn has_voted(env: Env, proposal_id: u64, voter: Address) -> bool {
        let key = DataKey::HasVoted(proposal_id, voter);
        let voted: bool = env.storage().persistent().get(&key).unwrap_or(false);
        if voted {
            bump_persistent(&env, &key);
        }
        voted
    }

    /// Cast a vote on an active proposal.
    ///
    /// Double-voting is strictly prohibited by tracking vote status per address in persistent storage.
    pub fn cast_vote(
        env: Env,
        voter: Address,
        proposal_id: u64,
        support: VoteType,
    ) -> Result<i128, Error> {
        voter.require_auth();
        bump_instance(&env);

        let current_state = Self::state(env.clone(), proposal_id)?;
        if current_state != ProposalState::Active {
            return Err(Error::ProposalNotActive);
        }

        // Double-voting shield: Check persistent storage
        let voted_key = DataKey::HasVoted(proposal_id, voter.clone());
        if env.storage().persistent().has(&voted_key) {
            return Err(Error::AlreadyVoted);
        }

        let weight = Self::get_staked_balance(env.clone(), voter.clone());
        if weight <= 0 {
            return Err(Error::ZeroVotingPower);
        }

        let prop_key = DataKey::Proposal(proposal_id);
        let mut proposal = Self::get_proposal(env.clone(), proposal_id)?;

        match support {
            VoteType::For => {
                proposal.for_votes = proposal
                    .for_votes
                    .checked_add(weight)
                    .ok_or(Error::Overflow)?;
            }
            VoteType::Against => {
                proposal.against_votes = proposal
                    .against_votes
                    .checked_add(weight)
                    .ok_or(Error::Overflow)?;
            }
            VoteType::Abstain => {
                proposal.abstain_votes = proposal
                    .abstain_votes
                    .checked_add(weight)
                    .ok_or(Error::Overflow)?;
            }
        }

        // Persist double-voting lock
        env.storage().persistent().set(&voted_key, &true);
        bump_persistent(&env, &voted_key);

        let user_vote_key = DataKey::UserVote(proposal_id, voter.clone());
        env.storage().persistent().set(&user_vote_key, &support);
        bump_persistent(&env, &user_vote_key);

        // Update proposal tallies
        env.storage().persistent().set(&prop_key, &proposal);
        bump_persistent(&env, &prop_key);

        Ok(weight)
    }

    /// Execute a succeeded proposal.
    ///
    /// Updates predefined administrative parameters according to the proposal action.
    pub fn execute(env: Env, proposal_id: u64) -> Result<(), Error> {
        bump_instance(&env);

        let current_state = Self::state(env.clone(), proposal_id)?;
        if current_state != ProposalState::Succeeded {
            return Err(Error::ProposalNotSucceeded);
        }

        let prop_key = DataKey::Proposal(proposal_id);
        let mut proposal = Self::get_proposal(env.clone(), proposal_id)?;

        if proposal.executed {
            return Err(Error::ProposalAlreadyExecuted);
        }

        // Mark executed
        proposal.executed = true;
        env.storage().persistent().set(&prop_key, &proposal);
        bump_persistent(&env, &prop_key);

        // Execute proposed action on administrative parameters
        match proposal.action {
            ProposalAction::UpdateFeeStructure(new_bps) => {
                env.storage()
                    .instance()
                    .set(&DataKey::FeeStructureBps, &new_bps);
            }
            ProposalAction::AddAmmPair((token_a, token_b)) => {
                let mut pairs: Vec<(Address, Address)> = env
                    .storage()
                    .instance()
                    .get(&DataKey::AmmPairs)
                    .unwrap_or(Vec::new(&env));
                pairs.push_back((token_a, token_b));
                env.storage().instance().set(&DataKey::AmmPairs, &pairs);
            }
            ProposalAction::UpdateProposalThreshold(new_threshold) => {
                let mut config = Self::get_config(env.clone())?;
                config.proposal_threshold = new_threshold;
                env.storage().instance().set(&DataKey::Config, &config);
            }
        }

        Ok(())
    }

    /// Retrieve the currently active fee structure in basis points.
    pub fn get_fee_structure(env: Env) -> u32 {
        bump_instance(&env);
        env.storage()
            .instance()
            .get(&DataKey::FeeStructureBps)
            .unwrap_or(30)
    }

    /// Retrieve approved AMM trading pairs.
    pub fn get_amm_pairs(env: Env) -> Vec<(Address, Address)> {
        bump_instance(&env);
        env.storage()
            .instance()
            .get(&DataKey::AmmPairs)
            .unwrap_or(Vec::new(&env))
    }
}

#[cfg(test)]
mod test;
