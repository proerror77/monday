use std::io::BufRead;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventEnvelope {
    #[serde(alias = "timestamp")]
    pub ts: i64,
    #[serde(default)]
    pub sequence: Option<u64>,
    #[serde(flatten)]
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventPayload {
    #[serde(alias = "snapshot")]
    Snapshot { bids: Vec<Level>, asks: Vec<Level> },
    #[serde(alias = "l2_update")]
    L2Update { bids: Vec<Level>, asks: Vec<Level> },
    #[serde(alias = "trade")]
    Trade {
        side: TradeSide,
        price: f64,
        quantity: f64,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TradeSide {
    #[serde(alias = "BUY")]
    Buy,
    #[serde(alias = "SELL")]
    Sell,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Level {
    pub price: f64,
    pub quantity: f64,
}

impl<'de> Deserialize<'de> for Level {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Array([f64; 2]),
            StringArray([String; 2]),
            Object { price: f64, quantity: f64 },
            ObjectAlt { price: f64, qty: f64 },
            ObjectSide { p: f64, q: f64 },
        }

        let level = match Repr::deserialize(deserializer)? {
            Repr::Array([price, quantity]) => Level { price, quantity },
            Repr::StringArray([price, quantity]) => Level {
                price: price.parse().map_err(serde::de::Error::custom)?,
                quantity: quantity.parse().map_err(serde::de::Error::custom)?,
            },
            Repr::Object { price, quantity } => Level { price, quantity },
            Repr::ObjectAlt { price, qty } => Level {
                price,
                quantity: qty,
            },
            Repr::ObjectSide { p, q } => Level {
                price: p,
                quantity: q,
            },
        };
        if !level.price.is_finite()
            || level.price <= 0.0
            || !level.quantity.is_finite()
            || level.quantity < 0.0
        {
            return Err(serde::de::Error::custom("invalid L2 price or quantity"));
        }
        Ok(level)
    }
}

pub struct EventStream<R: BufRead> {
    reader: std::io::Lines<R>,
    line_no: usize,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
    require_sequence: bool,
    next_sequence: Option<u64>,
    last_ts: Option<i64>,
}

impl<R: BufRead> EventStream<R> {
    pub fn new(
        reader: R,
        start_ts: Option<i64>,
        end_ts: Option<i64>,
        require_sequence: bool,
    ) -> Self {
        Self {
            reader: reader.lines(),
            line_no: 0,
            start_ts,
            end_ts,
            require_sequence,
            next_sequence: None,
            last_ts: None,
        }
    }
}

impl<R: BufRead> Iterator for EventStream<R> {
    type Item = anyhow::Result<EventEnvelope>;

    fn next(&mut self) -> Option<Self::Item> {
        for line in self.reader.by_ref() {
            self.line_no += 1;
            match line {
                Ok(ref raw) if raw.trim().is_empty() => continue,
                Ok(raw) => {
                    let event: EventEnvelope = match serde_json::from_str(&raw) {
                        Ok(ev) => ev,
                        Err(err) => {
                            let error =
                                anyhow::anyhow!("解析事件失敗 (line {}): {}", self.line_no, err);
                            return Some(Err(error));
                        }
                    };

                    if self.require_sequence {
                        let sequence = match event.sequence {
                            Some(sequence) => sequence,
                            None => {
                                return Some(Err(anyhow::anyhow!(
                                    "事件缺少 sequence (line {})",
                                    self.line_no
                                )))
                            }
                        };
                        if let Some(expected) = self.next_sequence {
                            if sequence != expected {
                                return Some(Err(anyhow::anyhow!(
                                    "事件 sequence gap (line {}): expected {}, actual {}",
                                    self.line_no,
                                    expected,
                                    sequence
                                )));
                            }
                        }
                        self.next_sequence = match sequence.checked_add(1) {
                            Some(next) => Some(next),
                            None => {
                                return Some(Err(anyhow::anyhow!(
                                    "事件 sequence overflow (line {})",
                                    self.line_no
                                )))
                            }
                        };
                    }
                    if self.last_ts.is_some_and(|last| event.ts < last) {
                        return Some(Err(anyhow::anyhow!(
                            "事件时间倒退 (line {}): previous {}, actual {}",
                            self.line_no,
                            self.last_ts.unwrap_or_default(),
                            event.ts
                        )));
                    }
                    self.last_ts = Some(event.ts);

                    if let Some(start) = self.start_ts {
                        if event.ts < start {
                            continue;
                        }
                    }
                    if let Some(end) = self.end_ts {
                        if event.ts > end {
                            return None;
                        }
                    }

                    return Some(Ok(event));
                }
                Err(err) => {
                    return Some(Err(err.into()));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn required_sequence_rejects_gaps() {
        let rows = concat!(
            "{\"ts\":1,\"sequence\":4,\"event\":\"snapshot\",\"bids\":[[1,1]],\"asks\":[[2,1]]}\n",
            "{\"ts\":2,\"sequence\":6,\"event\":\"trade\",\"side\":\"buy\",\"price\":1,\"quantity\":1}\n"
        );
        let mut stream = EventStream::new(Cursor::new(rows), None, None, true);
        assert!(stream.next().expect("first row").is_ok());
        assert!(stream
            .next()
            .expect("gap row")
            .expect_err("gap must fail")
            .to_string()
            .contains("sequence gap"));
    }
}
