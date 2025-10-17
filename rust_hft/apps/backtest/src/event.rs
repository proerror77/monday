use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct EventEnvelope {
    #[serde(alias = "timestamp")]
    pub ts: i64,
    #[serde(flatten)]
    pub payload: EventPayload,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TradeSide {
    #[serde(alias = "BUY")]
    Buy,
    #[serde(alias = "SELL")]
    Sell,
}

#[derive(Debug, Clone, Copy)]
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
            Object { price: f64, quantity: f64 },
            ObjectAlt { price: f64, qty: f64 },
            ObjectSide { p: f64, q: f64 },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Array([price, quantity]) => Ok(Level { price, quantity }),
            Repr::Object { price, quantity } => Ok(Level { price, quantity }),
            Repr::ObjectAlt { price, qty } => Ok(Level { price, quantity: qty }),
            Repr::ObjectSide { p, q } => Ok(Level { price: p, quantity: q }),
        }
    }
}

pub struct EventStream<R: BufRead> {
    reader: std::io::Lines<R>,
    line_no: usize,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
}

impl<R: BufRead> EventStream<R> {
    pub fn new(reader: R, start_ts: Option<i64>, end_ts: Option<i64>) -> Self {
        Self {
            reader: reader.lines(),
            line_no: 0,
            start_ts,
            end_ts,
        }
    }

    pub fn from_path<P: AsRef<Path>>(
        path: P,
        start_ts: Option<i64>,
        end_ts: Option<i64>,
    ) -> anyhow::Result<EventStream<BufReader<File>>> {
        let file = File::open(&path)
            .with_context(|| format!("無法開啟事件檔案: {}", path.as_ref().display()))?;
        Ok(EventStream::new(BufReader::new(file), start_ts, end_ts))
    }
}

impl<R: BufRead> Iterator for EventStream<R> {
    type Item = anyhow::Result<EventEnvelope>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(line) = self.reader.next() {
            self.line_no += 1;
            match line {
                Ok(ref raw) if raw.trim().is_empty() => continue,
                Ok(raw) => {
                    let event: EventEnvelope = match serde_json::from_str(&raw) {
                        Ok(ev) => ev,
                        Err(err) => {
                            let error = anyhow::anyhow!(
                                "解析事件失敗 (line {}): {}",
                                self.line_no,
                                err
                            );
                            return Some(Err(error));
                        }
                    };

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

pub fn open_event_stream<P: AsRef<Path>>(
    path: P,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
) -> anyhow::Result<EventStream<BufReader<File>>> {
    EventStream::<BufReader<File>>::from_path(path, start_ts, end_ts)
}
