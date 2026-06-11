// ...

impl ChainSync {
    // ...

    pub fn fork_switch(&mut self, new_tip: HeaderHash, intersection: HeaderHash) {
        // ...

        // Republish the ledger view after the rollback
        self.ledger_view = self.build_ledger_view();
        self.publish_ledger_view();

        // ...
    }

    // ...

    fn build_ledger_view(&self) -> LedgerView {
        // ...
    }

    fn publish_ledger_view(&self) {
        // ...
    }
}

// ...