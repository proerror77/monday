use crate::factors::FactorObservation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Buy,
    Sell,
    Hold,
}

pub trait SignalSource: Send + Sync {
    fn signal(&self, obs: &FactorObservation) -> Signal;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysBuy;
    impl SignalSource for AlwaysBuy {
        fn signal(&self, _obs: &crate::factors::FactorObservation) -> Signal {
            Signal::Buy
        }
    }

    #[test]
    fn signal_source_is_object_safe() {
        let _src: Box<dyn SignalSource> = Box::new(AlwaysBuy);
    }
}
