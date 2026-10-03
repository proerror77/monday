-- Named parameters are bound by the CH HTTP client. Common numerical features
-- are computed once over the admitted sources and lookback, not for each trial.
INSERT INTO research.prepared_features
SELECT {view_id:String}, segment, ordinal, event_ns, available_ns,
       [(bids_price[1]+asks_price[1])/2,
        asks_price[1]-bids_price[1],
        (arraySum(arraySlice(bids_quantity,1,{depth:UInt16}))-
         arraySum(arraySlice(asks_quantity,1,{depth:UInt16})))/
        nullIf(arraySum(arraySlice(bids_quantity,1,{depth:UInt16}))+
               arraySum(arraySlice(asks_quantity,1,{depth:UInt16})),0)] AS values
FROM research.normalized_books
WHERE venue={venue:String} AND instrument={instrument:String} AND market={market:String}
  AND source_sha256 IN {sources:Array(String)}
  AND available_ns >= {start_ns:Int64}-{lookback_ns:Int64}
  AND available_ns < {end_ns:Int64}
  AND length(bids_price)>0 AND length(asks_price)>0
  AND arraySum(arraySlice(bids_quantity,1,{depth:UInt16}))+arraySum(arraySlice(asks_quantity,1,{depth:UInt16}))>0
ORDER BY available_ns, segment, ordinal;

-- One temporal join prepares every requested horizon. Label target is based
-- on the anchor's AVAILABLE clock, not array offset or a resampling frequency.
-- Equality segment disallows crossing a gap/session. The split-end bound
-- excludes labels that reach into the next split. The maturity bound excludes
-- future information unavailable at the admitted fitting cutoff.
INSERT INTO research.prepared_labels
WITH anchors AS (
  SELECT view_id,segment,ordinal,available_ns,values[1] AS anchor_mid,
         arrayJoin({horizons_ns:Array(Int64)}) AS horizon_ns
  FROM research.prepared_features
  WHERE view_id={view_id:String} AND available_ns >= {start_ns:Int64}
    AND available_ns < {end_ns:Int64}
), future AS (
  -- ASOF ties are collapsed deterministically before joining. MergeTree ORDER
  -- BY is not a uniqueness guarantee and cannot choose a label tie for us.
  SELECT segment,event_ns,
         argMin(n.available_ns,tuple(n.available_ns,n.ordinal,n.source_sha256)) AS available_ns,
         argMin((n.bids_price[1]+n.asks_price[1])/2,tuple(n.available_ns,n.ordinal,n.source_sha256)) AS target_mid
  FROM research.normalized_books n
  WHERE venue={venue:String} AND instrument={instrument:String} AND market={market:String}
    AND source_sha256 IN {sources:Array(String)}
    AND event_ns < {end_ns:Int64} AND available_ns <= {fit_cutoff_ns:Int64}
    AND length(bids_price)>0 AND length(asks_price)>0
  GROUP BY segment,event_ns
  ORDER BY segment,event_ns
)
SELECT a.view_id,a.segment,a.ordinal,a.horizon_ns,f.event_ns,f.available_ns,
       f.target_mid/a.anchor_mid-1
FROM anchors a ASOF INNER JOIN future f
 ON a.segment=f.segment AND a.available_ns+a.horizon_ns <= f.event_ns
WHERE f.event_ns <= a.available_ns+a.horizon_ns+{label_tolerance_ns:Int64}
  AND f.available_ns >= f.event_ns
ORDER BY a.segment,a.ordinal,a.horizon_ns;
