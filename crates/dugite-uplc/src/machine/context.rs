//! CEK continuation stack.

use crate::machine::env::Env;
use crate::machine::value::Value;
use crate::term::Term;

#[derive(Debug, Clone)]
pub enum Frame {
    AwaitArg { function: Value, env: Env },
    AwaitFunTerm { argument: Term, env: Env },
    Force,
}

#[derive(Debug, Clone, Default)]
pub struct Kont {
    frames: Vec<Frame>,
}

impl Kont {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, f: Frame) -> Result<(), crate::UplcError> {
        if self.frames.len() >= super::MAX_KONTINUATION_DEPTH {
            return Err(crate::UplcError::Internal(format!(
                "CEK continuation depth exceeds limit ({})",
                super::MAX_KONTINUATION_DEPTH
            )));
        }
        self.frames.push(f);
        Ok(())
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
        k.push(Frame::Force).unwrap();
        k.push(Frame::AwaitArg {
            function: int_val(1),
            env: Env::new(),
        })
        .unwrap();
        assert_eq!(k.depth(), 2);
        assert!(matches!(k.pop(), Some(Frame::AwaitArg { .. })));
        assert!(matches!(k.pop(), Some(Frame::Force)));
        assert!(k.pop().is_none());
    }

    #[test]
    fn rejects_overdeep_push() {
        let mut k = Kont::new();
        for _ in 0..super::super::MAX_KONTINUATION_DEPTH {
            k.push(Frame::Force).unwrap();
        }
        let err = k.push(Frame::Force).unwrap_err();
        assert!(matches!(err, crate::UplcError::Internal(_)));
    }
}
