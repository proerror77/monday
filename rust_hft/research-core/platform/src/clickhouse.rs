//! Fixed, parameter-bound SQL over verified normalized data. Each preparation
//! attempt writes a distinct physical generation; publication remains in PG.
use crate::{
    data::{
        DataViewSpec, Exit, FeatureFrame, Label, PublishedView, ReplayEvent, ReplayPayload, Split,
        TrainingFrame, TypedBlock, PREPARE_SQL,
    },
    postgres::PreparationPermit,
};
use anyhow::{ensure, Context, Result};

pub struct ClickHouse {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    user: String,
    password: String,
}
pub type Cursor = (i64, String, u64);
impl ClickHouse {
    pub fn new(endpoint: &str, user: String, password: String) -> Result<Self> {
        let endpoint = reqwest::Url::parse(endpoint)?;
        ensure!(
            endpoint.scheme() == "https"
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none(),
            "invalid ClickHouse endpoint"
        );
        ensure!(!user.is_empty(), "ClickHouse identity required");
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            endpoint,
            user,
            password,
        })
    }
    pub fn parameters(spec: &DataViewSpec) -> Result<Vec<(&'static str, String)>> {
        spec.validate()?;
        Ok(vec![
            ("param_venue", spec.venue.clone()),
            ("param_instrument", spec.instrument.clone()),
            ("param_market", spec.market.clone()),
            ("param_depth", spec.depth.to_string()),
            ("param_start_ns", spec.window.start_ns.to_string()),
            ("param_end_ns", spec.window.end_ns.to_string()),
            ("param_lookback_ns", spec.lookback_ns.to_string()),
            (
                "param_label_tolerance_ns",
                spec.label_tolerance_ns.to_string(),
            ),
            ("param_fit_cutoff_ns", spec.fit_cutoff_ns.to_string()),
            (
                "param_sources",
                format!(
                    "[{}]",
                    spec.sources
                        .iter()
                        .map(|s| format!("'{s}'"))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            ),
            (
                "param_horizons_ns",
                format!(
                    "[{}]",
                    spec.horizons_ns
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            ),
        ])
    }
    async fn query(
        &self,
        spec: &DataViewSpec,
        generation: &str,
        sql: &str,
        extra: Vec<(&str, String)>,
        max: usize,
    ) -> Result<Vec<u8>> {
        ensure!(
            crate::valid_digest(generation),
            "invalid physical generation"
        );
        let mut url = self.endpoint.clone();
        for (name, value) in Self::parameters(spec)? {
            url.query_pairs_mut().append_pair(name, &value);
        }
        url.query_pairs_mut()
            .append_pair("param_view_id", generation)
            .append_pair("max_execution_time", "45")
            .append_pair("timeout_overflow_mode", "throw");
        for (name, value) in extra {
            url.query_pairs_mut().append_pair(name, &value);
        }
        let mut response = self
            .client
            .post(url)
            .basic_auth(&self.user, Some(&self.password))
            .body(sql.to_owned())
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("ClickHouse transport unavailable"))?
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("ClickHouse query rejected"))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("ClickHouse read interrupted"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= max,
                "ClickHouse response exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
    pub async fn prepare(&self, permit: &mut PreparationPermit) -> Result<()> {
        permit.check().await?;
        let spec = permit.plan().spec.clone();
        let generation = permit.generation().to_owned();
        ensure!(
            spec.split == Split::Train,
            "label preparation cannot open sealed evaluation data"
        );
        // A retry has another generation. Never DELETE a published generation
        // or overwrite rows written by an old, disconnected worker.
        let existing=self.query(&spec,&generation,"SELECT count() FROM research.prepared_features WHERE view_id={view_id:String} FORMAT TabSeparated",vec![],1024).await?;
        ensure!(
            std::str::from_utf8(&existing)?.trim() == "0",
            "generation already materialized; duplicate worker rejected"
        );
        let source=self.query(&spec,&generation,"SELECT count(),uniqExact(tuple(segment,ordinal)) FROM research.normalized_books WHERE venue={venue:String} AND instrument={instrument:String} AND market={market:String} AND source_sha256 IN {sources:Array(String)} AND available_ns>={start_ns:Int64}-{lookback_ns:Int64} AND available_ns<{end_ns:Int64} FORMAT TabSeparated",vec![],1024).await?;
        unique_coverage(&source)?;
        let recipe = permit
            .plan()
            .recipe_sql
            .as_deref()
            .unwrap_or(PREPARE_SQL)
            .to_owned();
        let statements: Vec<_> = recipe
            .split(';')
            .filter(|q| q.contains("INSERT INTO"))
            .collect();
        ensure!(statements.len() == 2, "invalid preparation recipe");
        for statement in statements {
            permit.check().await?;
            self.query(&spec, &generation, statement, vec![], 1024)
                .await?;
        }
        for sql in ["SELECT count(),uniqExact(tuple(segment,ordinal)) FROM research.prepared_features WHERE view_id={view_id:String} FORMAT TabSeparated","SELECT count(),uniqExact(tuple(segment,ordinal,horizon_ns)) FROM research.prepared_labels WHERE view_id={view_id:String} FORMAT TabSeparated"] {
            permit.check().await?;unique_coverage(&self.query(&spec,&generation,sql,vec![],1024).await?)?;
        }
        permit.check().await
    }
    /// Publisher exit reads require a live opaque preparation permit.
    pub async fn preparation_block(
        &self,
        permit: &mut PreparationPermit,
        exit: Exit,
        after: Option<Cursor>,
        rows: u16,
    ) -> Result<TypedBlock> {
        permit.check().await?;
        self.block(&permit.plan().spec, permit.generation(), exit, after, rows)
            .await
    }
    /// Consumer reads require the manifest SHA from the authoritative PG record.
    pub async fn published_block(
        &self,
        view: &PublishedView,
        expected: &str,
        exit: Exit,
        after: Option<Cursor>,
        rows: u16,
    ) -> Result<TypedBlock> {
        view.verify(expected)?;
        self.block(&view.spec, &view.prepared_id, exit, after, rows)
            .await
    }
    async fn block(
        &self,
        spec: &DataViewSpec,
        generation: &str,
        exit: Exit,
        after: Option<Cursor>,
        rows: u16,
    ) -> Result<TypedBlock> {
        ensure!(rows > 0 && rows <= 4096, "invalid row limit");
        let (clock, segment, ordinal) = after.unwrap_or((0, String::new(), 0));
        let extra = vec![
            ("param_cursor_ns", clock.to_string()),
            ("param_cursor_segment", segment),
            ("param_cursor_ordinal", ordinal.to_string()),
            ("param_limit", rows.to_string()),
        ];
        let sql=match exit {
            Exit::Features=>"SELECT segment,ordinal,event_ns,available_ns,values FROM research.prepared_features WHERE view_id={view_id:String} AND tuple(available_ns,segment,ordinal)>tuple({cursor_ns:Int64},{cursor_segment:String},{cursor_ordinal:UInt64}) ORDER BY available_ns,segment,ordinal LIMIT {limit:UInt16} FORMAT RowBinary",
            Exit::Training=>"SELECT f.segment,f.ordinal,f.event_ns,f.available_ns,f.values,arraySort(x->x.1,groupArray(tuple(l.horizon_ns,l.target_event_ns,l.mature_ns,l.value))) AS labels FROM research.prepared_features f INNER JOIN research.prepared_labels l ON f.view_id=l.view_id AND f.segment=l.segment AND f.ordinal=l.ordinal WHERE f.view_id={view_id:String} AND f.available_ns>={start_ns:Int64} AND tuple(f.available_ns,f.segment,f.ordinal)>tuple({cursor_ns:Int64},{cursor_segment:String},{cursor_ordinal:UInt64}) GROUP BY f.segment,f.ordinal,f.event_ns,f.available_ns,f.values HAVING arrayMap(x->x.1,labels)={horizons_ns:Array(Int64)} ORDER BY f.available_ns,f.segment,f.ordinal LIMIT {limit:UInt16} FORMAT RowBinary",
            Exit::Replay=>"SELECT segment,ordinal,event_ns,available_ns,toUInt8(kind),arrayZip(bids_price,bids_quantity),arrayZip(asks_price,asks_quantity),trade_price,trade_quantity,buyer_initiated FROM research.normalized_events WHERE venue={venue:String} AND instrument={instrument:String} AND market={market:String} AND source_sha256 IN {sources:Array(String)} AND available_ns>={start_ns:Int64}-{lookback_ns:Int64} AND available_ns<{end_ns:Int64} AND tuple(available_ns,segment,ordinal)>tuple({cursor_ns:Int64},{cursor_segment:String},{cursor_ordinal:UInt64}) ORDER BY available_ns,segment,ordinal LIMIT {limit:UInt16} FORMAT RowBinary",
        };
        let bytes = self
            .query(spec, generation, sql, extra, 16 * 1024 * 1024)
            .await?;
        decode_rows(&bytes, exit, spec, rows)
    }
}
fn unique_coverage(bytes: &[u8]) -> Result<()> {
    let counts: Vec<u64> = std::str::from_utf8(bytes)?
        .trim()
        .split('\t')
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    ensure!(
        counts.len() == 2 && counts[0] > 0 && counts[0] == counts[1],
        "source/prepared coverage empty or duplicated"
    );
    Ok(())
}
fn decode_rows(bytes: &[u8], exit: Exit, spec: &DataViewSpec, limit: u16) -> Result<TypedBlock> {
    let mut wire = RowBinary { bytes, pos: 0 };
    let mut features = Vec::new();
    let mut training = Vec::new();
    let mut replay = Vec::new();
    let mut count = 0;
    while wire.pos < bytes.len() {
        ensure!(count < usize::from(limit), "too many rows");
        count += 1;
        let segment = wire.string()?;
        let ordinal = wire.u64()?;
        let event_ns = wire.u64()? as i64;
        let available_ns = wire.u64()? as i64;
        if exit == Exit::Replay {
            let kind = wire.byte()?;
            let bids = wire.levels(spec.depth)?;
            let asks = wire.levels(spec.depth)?;
            let price = wire.float()?;
            let quantity = wire.float()?;
            let buyer = wire.byte()?;
            ensure!(buyer <= 1, "invalid trade side");
            let payload = match kind {
                1 => ReplayPayload::Snapshot { bids, asks },
                2 => ReplayPayload::Delta { bids, asks },
                3 => ReplayPayload::Trade {
                    price,
                    quantity,
                    buyer_initiated: buyer == 1,
                },
                _ => anyhow::bail!("unknown event type"),
            };
            replay.push(ReplayEvent {
                segment,
                ordinal,
                event_ns,
                available_ns,
                payload,
            });
        } else {
            let len = wire.varint()?;
            ensure!(len == spec.feature_names.len(), "feature schema changed");
            let values = (0..len).map(|_| wire.float()).collect::<Result<Vec<_>>>()?;
            let feature = FeatureFrame {
                segment,
                ordinal,
                event_ns,
                available_ns,
                values,
            };
            if exit == Exit::Features {
                features.push(feature);
            } else {
                let len = wire.varint()?;
                ensure!(len == spec.horizons_ns.len(), "incomplete horizon set");
                let mut labels = Vec::with_capacity(len);
                for _ in 0..len {
                    labels.push(Label {
                        horizon_ns: wire.u64()? as i64,
                        target_event_ns: wire.u64()? as i64,
                        mature_ns: wire.u64()? as i64,
                        value: wire.float()?,
                    });
                }
                training.push(TrainingFrame { feature, labels });
            }
        }
    }
    Ok(match exit {
        Exit::Features => TypedBlock::Features(features),
        Exit::Training => TypedBlock::Training(training),
        Exit::Replay => TypedBlock::Replay(replay),
    })
}
struct RowBinary<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl RowBinary<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.bytes.get(self.pos).context("truncated RowBinary")?;
        self.pos += 1;
        Ok(b)
    }
    fn u64(&mut self) -> Result<u64> {
        let end = self.pos.checked_add(8).context("wire overflow")?;
        let v = u64::from_le_bytes(
            self.bytes
                .get(self.pos..end)
                .context("truncated RowBinary")?
                .try_into()?,
        );
        self.pos = end;
        Ok(v)
    }
    fn float(&mut self) -> Result<f64> {
        let v = f64::from_bits(self.u64()?);
        ensure!(v.is_finite(), "nonfinite RowBinary number");
        Ok(v)
    }
    fn varint(&mut self) -> Result<usize> {
        let mut v = 0usize;
        for shift in (0..63).step_by(7) {
            let b = self.byte()?;
            v |= usize::from(b & 127)
                .checked_shl(shift)
                .context("length overflow")?;
            if b & 128 == 0 {
                return Ok(v);
            }
        }
        anyhow::bail!("length overflow")
    }
    fn string(&mut self) -> Result<String> {
        let n = self.varint()?;
        ensure!(n <= 4096, "oversized string");
        let end = self.pos.checked_add(n).context("wire overflow")?;
        let s = std::str::from_utf8(self.bytes.get(self.pos..end).context("truncated string")?)?
            .to_owned();
        self.pos = end;
        Ok(s)
    }
    fn levels(&mut self, depth: u16) -> Result<Vec<[f64; 2]>> {
        let n = self.varint()?;
        ensure!(n <= usize::from(depth), "event depth exceeds contract");
        (0..n).map(|_| Ok([self.float()?, self.float()?])).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256;
    fn spec() -> DataViewSpec {
        DataViewSpec {
            schema: 1,
            venue: "fixture".into(),
            market: "usdm".into(),
            instrument: "fixture".into(),
            depth: 2,
            sources: vec!["a".repeat(64)],
            normalizer_sha256: "b".repeat(64),
            feature_sql_sha256: sha256(PREPARE_SQL.as_bytes()),
            feature_names: vec!["mid".into(), "spread".into(), "depth_imbalance".into()],
            window: crate::data::Window {
                start_ns: 100,
                end_ns: 1000,
            },
            lookback_ns: 50,
            horizons_ns: vec![70, 150],
            label_tolerance_ns: 20,
            fit_cutoff_ns: 1000,
            split: Split::Train,
        }
    }
    fn feature() -> Vec<u8> {
        let mut bytes = vec![1, b'A'];
        for n in [1u64, 199, 200] {
            bytes.extend(n.to_le_bytes());
        }
        bytes.push(3);
        for v in [101.0f64, 2.0, 0.0] {
            bytes.extend(v.to_le_bytes());
        }
        bytes
    }
    #[test]
    fn rowbinary_rejects_truncation_nonfinite_schema_and_row_overrun() {
        let bytes = feature();
        let TypedBlock::Features(rows) = decode_rows(&bytes, Exit::Features, &spec(), 1).unwrap()
        else {
            panic!("wrong exit")
        };
        assert_eq!(rows[0].values, vec![101.0, 2.0, 0.0]);
        assert_eq!(rows[0].available_ns, 200);
        assert!(decode_rows(&bytes[..bytes.len() - 1], Exit::Features, &spec(), 1).is_err());
        let mut wrong = bytes.clone();
        wrong[26] = 4;
        assert!(decode_rows(&wrong, Exit::Features, &spec(), 1).is_err());
        let mut nonfinite = bytes.clone();
        nonfinite[27..35].copy_from_slice(&f64::NAN.to_le_bytes());
        assert!(decode_rows(&nonfinite, Exit::Features, &spec(), 1).is_err());
        let double = [bytes.clone(), bytes].concat();
        assert!(decode_rows(&double, Exit::Features, &spec(), 1).is_err());
        assert!(decode_rows(&[255; 12], Exit::Features, &spec(), 1).is_err());
    }
    #[test]
    #[ignore = "requires the disposable CH SQL fixture's RowBinary file"]
    fn actual_clickhouse_rowbinary_preserves_typed_feature_contract() {
        let path = std::env::var("MONDAY_TEST_CH_ROWBINARY").expect("fixture output path required");
        let bytes = std::fs::read(path).unwrap();
        let block = decode_rows(&bytes, Exit::Features, &spec(), 16).unwrap();
        let TypedBlock::Features(rows) = &block else {
            panic!("wrong exit")
        };
        assert_eq!(rows.len(), 7);
        let anchor = rows
            .iter()
            .find(|r| r.segment == "A" && r.ordinal == 1)
            .unwrap();
        assert_eq!(anchor.values, vec![101.0, 2.0, 0.0]);
        assert_eq!(anchor.event_ns, 199);
        assert_eq!(anchor.available_ns, 200);
        let encoded = crate::prepared::encode(&block).unwrap();
        let reference = crate::data::BlockRef {
            sha256: sha256(&encoded),
            bytes: encoded.len() as u64,
            rows: 7,
            decoded_bytes: 65536,
            exit: Exit::Features,
        };
        crate::data::validate_block(&block, &reference, &spec()).unwrap();
    }
}
