//! CEK continuation stack.

use crate::machine::env::Env;
use crate::machine::value::Value;
use crate::term::Term;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Frame {
    AwaitArg {
        function: Value,
        env: Env,
    },
    AwaitFunTerm {
        argument: Rc<Term>,
        env: Env,
    },
    Force,
    /// `ApplyValue arg` — when the current return value lands, apply
    /// it to `arg` (which is already an evaluated `Value`). Used by
    /// `Case` dispatch to apply the matching branch to the
    /// Constr-payload values in left-to-right order.
    ApplyValue {
        argument: Value,
    },
    /// `Constr tag pending_args evaluated_args env` — we're evaluating
    /// the arguments of a `Constr` left-to-right. The next pending arg
    /// is at `pending_args.front()`; already-evaluated args are kept
    /// in order in `evaluated_args`.
    ///
    /// We use `Vec<Term>` for the pending list (popping from the front
    /// via swap-remove or by tracking an offset would be cheaper, but
    /// the SoP arg-count is small).
    Constr {
        tag: u64,
        pending: Vec<Rc<Term>>,
        evaluated: Vec<Value>,
        env: Env,
    },
    /// `Case branches env` — the scrutinee has been reduced to a
    /// `Constr`; we pick a branch from `branches` indexed by the
    /// constr tag, apply the constr args, and evaluate.
    Cases {
        branches: Vec<Rc<Term>>,
        env: Env,
    },
}

#[derive(Debug, Clone, Default)]
pub struct Kont {
    frames: Vec<Frame>,
}

impl Kont {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, f: Frame) {
        self.frames.push(f);
    }

    pub fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Constant;
    use num_bigint::BigInt;

    fn int_val(n: i64) -> Value {
        Value::Const(Constant::Integer(BigInt::from(n)))
    }

    #[test]
    fn push_pop_lifo() {
        let mut k = Kont::new();
        k.push(Frame::Force);
        k.push(Frame::AwaitArg {
            function: int_val(1),
            env: Env::new(),
        });
        assert_eq!(k.depth(), 2);
        assert!(matches!(k.pop(), Some(Frame::AwaitArg { .. })));
        assert!(matches!(k.pop(), Some(Frame::Force)));
        assert!(k.pop().is_none());
    }

    #[test]
    fn push_beyond_former_hard_cap_succeeds() {
        // The CEK continuation stack is a heap-allocated `Vec<Frame>`
        // (no OS call-stack recursion), so depth is bounded only by
        // `ExBudget` exhaustion, exactly as in Haskell's `Context`
        // (`UntypedPlutusCore.Evaluation.Machine.Cek.Internal`), which
        // has no depth field at all. Pushing well past the former
        // 4096-frame cap must succeed (see #817).
        let mut k = Kont::new();
        for _ in 0..(4 * 1024 + 1000) {
            k.push(Frame::Force);
        }
        assert_eq!(k.depth(), 4 * 1024 + 1000);
    }
}
