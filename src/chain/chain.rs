use super::*;
use crate::block::Block;
use crate::database::Database;
use crate::error::Error;
use crate::header::Header;
use crate::reward::Reward;

pub struct Chain {
    // ...
    reward_state: Reward,
}

impl Chain {
    // ...

    pub fn recompute_reward_state(&mut self) -> Result<(), Error> {
        let reward_state = self.compute_reward_state()?;
        self.set_reward_state(reward_state)?;

        Ok(())
    }

    pub fn compute_reward_state(&self) -> Result<Reward, Error> {
        // Compute the reward/RUPD state
        let mut reward = Reward::new();
        for account in self.accounts() {
            // ...
        }
        Ok(reward)
    }

    pub fn set_reward_state(&mut self, reward_state: Reward) -> Result<(), Error> {
        self.reward_state = reward_state;
        Ok(())
    }
}