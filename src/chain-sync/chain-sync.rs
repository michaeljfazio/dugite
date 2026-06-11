use super::*;
use crate::block::Block;
use crate::chain::Chain;
use crate::database::Database;
use crate::error::Error;
use crate::header::Header;
use crate::reward::Reward;

pub fn apply_block(
    db: &mut Database,
    chain: &mut Chain,
    block: &Block,
    header: &Header,
) -> Result<(), Error> {
    // ...

    // Recompute the reward/RUPD state post-replay
    if chain.epoch() == 337 {
        chain.recompute_reward_state()?;
    }

    // ...
}

pub fn recompute_reward_state(chain: &mut Chain) -> Result<(), Error> {
    // Recompute the reward/RUPD state
    let reward_state = chain.compute_reward_state()?;
    chain.set_reward_state(reward_state)?;

    Ok(())
}

pub fn compute_reward_state(chain: &Chain) -> Result<Reward, Error> {
    // Compute the reward/RUPD state
    let mut reward = Reward::new();
    for account in chain.accounts() {
        // ...
    }
    Ok(reward)
}