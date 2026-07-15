use crate::model::traits::Transition;
use std::collections::VecDeque;

pub struct ReplayBuffer {
    buffer: VecDeque<Transition>,
    capacity: usize,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            capacity,
        }
    }
    pub fn push(&mut self, t: Transition) {
        if self.buffer.len() == self.capacity {
            self.buffer.pop_front();
        }
        self.buffer.push_back(t);
    }
    pub fn len(&self) -> usize {
        self.buffer.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
    pub fn sample(&self, n: usize, rng: &mut impl rand::Rng) -> Vec<&Transition> {
        if n == 0 || self.buffer.is_empty() {
            return vec![];
        }
        let count = n.min(self.buffer.len());
        rand::seq::index::sample(rng, self.buffer.len(), count)
            .into_iter()
            .map(|i| &self.buffer[i])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::traits::Transition;

    fn t(action: u8) -> Transition {
        Transition {
            state: vec![0.0],
            action,
            reward: 0.0,
            next_state: vec![0.0],
            done: false,
        }
    }

    #[test]
    fn sample_returns_correct_count() {
        let mut buf = ReplayBuffer::new(100);
        for i in 0..10u8 {
            buf.push(t(i));
        }
        let mut rng = rand::thread_rng();
        let s = buf.sample(5, &mut rng);
        assert_eq!(s.len(), 5);
    }

    #[test]
    fn sample_does_not_exceed_buffer_size() {
        let mut buf = ReplayBuffer::new(100);
        buf.push(t(0));
        let mut rng = rand::thread_rng();
        let s = buf.sample(10, &mut rng);
        assert_eq!(s.len(), 1);
    }
}
