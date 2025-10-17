"""
Data Cleaning and Continuity Analysis for HFT Market Data

This module analyzes data gaps and provides recommendations for:
1. Identifying continuous data segments suitable for training
2. Flagging problematic periods that should be excluded
3. Generating cleaned datasets with gap markers
"""

from datetime import datetime, timezone
from typing import List, Dict, Tuple
import sys
from pathlib import Path

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

from utils.clickhouse_client import ClickHouseClient
from utils.time import parse_iso8601


def analyze_continuity(
    client: ClickHouseClient,
    table: str,
    symbol: str,
    start_ms: int,
    end_ms: int,
    max_gap_sec: int = 60
) -> Dict:
    """
    Analyze data continuity and identify usable segments.

    Args:
        client: ClickHouse client
        table: Table name
        symbol: Trading symbol
        start_ms: Start timestamp in milliseconds
        end_ms: End timestamp in milliseconds
        max_gap_sec: Maximum acceptable gap in seconds (default: 60)

    Returns:
        Dict with continuous segments and quality metrics
    """
    # Query to get all timestamps
    query = f"""
    SELECT toInt64(toUnixTimestamp(ts)) AS ts_sec
    FROM {table}
    WHERE symbol = '{symbol}'
      AND toUnixTimestamp(ts) BETWEEN {start_ms // 1000} AND {end_ms // 1000}
    ORDER BY ts_sec
    """

    df = client.query_to_dataframe(query)
    if df is None or df.empty:
        return {
            'ok': False,
            'error': 'No data found',
            'segments': [],
            'total_gaps': 0,
            'usable_pct': 0.0
        }

    # Convert to integers
    timestamps = df['ts_sec'].astype(int).tolist()

    # Identify continuous segments
    segments = []
    current_start = timestamps[0]
    prev_ts = timestamps[0]

    for ts in timestamps[1:]:
        gap = ts - prev_ts
        if gap > max_gap_sec:
            # End current segment
            segments.append({
                'start': current_start,
                'end': prev_ts,
                'duration_sec': prev_ts - current_start,
                'gap_after': gap
            })
            current_start = ts
        prev_ts = ts

    # Add final segment
    segments.append({
        'start': current_start,
        'end': prev_ts,
        'duration_sec': prev_ts - current_start,
        'gap_after': 0
    })

    # Calculate quality metrics
    total_duration = sum(s['duration_sec'] for s in segments)
    total_time_range = (end_ms - start_ms) // 1000
    usable_pct = (total_duration / total_time_range) * 100

    # Classify segments
    for seg in segments:
        duration_hours = seg['duration_sec'] / 3600
        if duration_hours >= 4:
            seg['quality'] = 'excellent'  # >= 4 hours continuous
        elif duration_hours >= 1:
            seg['quality'] = 'good'       # 1-4 hours
        elif duration_hours >= 0.25:
            seg['quality'] = 'marginal'   # 15min - 1 hour
        else:
            seg['quality'] = 'poor'       # < 15 minutes

        seg['start_dt'] = datetime.fromtimestamp(seg['start'], tz=timezone.utc).isoformat()
        seg['end_dt'] = datetime.fromtimestamp(seg['end'], tz=timezone.utc).isoformat()
        seg['duration_hours'] = duration_hours

    return {
        'ok': True,
        'table': table,
        'symbol': symbol,
        'segments': segments,
        'total_segments': len(segments),
        'total_gaps': len([s for s in segments if s['gap_after'] > 0]),
        'usable_duration_sec': total_duration,
        'usable_duration_hours': total_duration / 3600,
        'total_time_range_sec': total_time_range,
        'usable_pct': usable_pct,
        'max_gap_sec': max_gap_sec
    }


def get_cleaning_recommendations(analysis: Dict) -> List[Dict]:
    """
    Generate cleaning recommendations based on continuity analysis.

    Returns:
        List of recommendations with actions
    """
    if not analysis['ok']:
        return [{'action': 'error', 'reason': analysis.get('error', 'Unknown error')}]

    recommendations = []
    segments = analysis['segments']

    # Recommend segments for training
    excellent_segs = [s for s in segments if s['quality'] == 'excellent']
    good_segs = [s for s in segments if s['quality'] == 'good']

    if excellent_segs:
        total_hours = sum(s['duration_hours'] for s in excellent_segs)
        recommendations.append({
            'action': 'use_for_training',
            'priority': 'high',
            'segments': len(excellent_segs),
            'total_hours': round(total_hours, 2),
            'reason': f'{len(excellent_segs)} excellent segments (>= 4 hours each) suitable for training'
        })

    if good_segs:
        total_hours = sum(s['duration_hours'] for s in good_segs)
        recommendations.append({
            'action': 'use_for_validation',
            'priority': 'medium',
            'segments': len(good_segs),
            'total_hours': round(total_hours, 2),
            'reason': f'{len(good_segs)} good segments (1-4 hours) suitable for validation'
        })

    # Identify problematic gaps
    large_gaps = [s for s in segments if s['gap_after'] > 3600]  # > 1 hour
    if large_gaps:
        recommendations.append({
            'action': 'exclude_gaps',
            'priority': 'critical',
            'gaps': len(large_gaps),
            'max_gap_hours': round(max(s['gap_after'] for s in large_gaps) / 3600, 2),
            'reason': f'{len(large_gaps)} large gaps (> 1 hour) should be excluded from training'
        })

    # Overall quality assessment
    usable_pct = analysis['usable_pct']
    if usable_pct < 20:
        recommendations.append({
            'action': 'collect_more_data',
            'priority': 'critical',
            'current_pct': round(usable_pct, 2),
            'reason': f'Only {usable_pct:.1f}% usable data - recommend collecting more continuous data'
        })
    elif usable_pct < 50:
        recommendations.append({
            'action': 'review_collection',
            'priority': 'high',
            'current_pct': round(usable_pct, 2),
            'reason': f'{usable_pct:.1f}% usable data - consider improving data collection reliability'
        })

    return recommendations


def print_analysis_report(analysis: Dict, recommendations: List[Dict]):
    """Print formatted analysis report."""
    print("\n" + "="*80)
    print("DATA CONTINUITY ANALYSIS REPORT")
    print("="*80)

    if not analysis['ok']:
        print(f"\n❌ ERROR: {analysis.get('error', 'Unknown error')}")
        return

    print(f"\nTable: {analysis['table']}")
    print(f"Symbol: {analysis['symbol']}")
    print(f"Max Gap Threshold: {analysis['max_gap_sec']} seconds")
    print(f"\nOverall Metrics:")
    print(f"  Total Segments: {analysis['total_segments']}")
    print(f"  Total Gaps: {analysis['total_gaps']}")
    print(f"  Usable Duration: {analysis['usable_duration_hours']:.2f} hours")
    print(f"  Usable Percentage: {analysis['usable_pct']:.2f}%")

    print("\n" + "-"*80)
    print("CONTINUOUS SEGMENTS")
    print("-"*80)

    for i, seg in enumerate(analysis['segments'], 1):
        quality_emoji = {
            'excellent': '🟢',
            'good': '🟡',
            'marginal': '🟠',
            'poor': '🔴'
        }

        print(f"\nSegment #{i} {quality_emoji.get(seg['quality'], '⚪')} {seg['quality'].upper()}")
        print(f"  Start: {seg['start_dt']}")
        print(f"  End:   {seg['end_dt']}")
        print(f"  Duration: {seg['duration_hours']:.2f} hours ({seg['duration_sec']:,} seconds)")
        if seg['gap_after'] > 0:
            print(f"  Gap After: {seg['gap_after']:,} seconds ({seg['gap_after']/3600:.2f} hours)")

    print("\n" + "-"*80)
    print("RECOMMENDATIONS")
    print("-"*80)

    for rec in recommendations:
        priority_emoji = {
            'critical': '🔴',
            'high': '🟡',
            'medium': '🟢',
            'low': '⚪'
        }

        emoji = priority_emoji.get(rec['priority'], '⚪')
        print(f"\n{emoji} {rec['action'].upper().replace('_', ' ')} [{rec['priority'].upper()}]")
        print(f"   {rec['reason']}")

        # Print additional details
        for key, value in rec.items():
            if key not in ['action', 'priority', 'reason']:
                print(f"   {key}: {value}")


def main():
    """Main entry point for data cleaning analysis."""
    import yaml
    from pathlib import Path
    import os

    # Load QA config
    config_path = Path(__file__).parent.parent.parent / "configs" / "qa.yaml"
    with open(config_path) as f:
        config = yaml.safe_load(f)

    # Initialize client (will auto-load from environment via utils.config)
    client = ClickHouseClient()

    # Parse time range
    start = parse_iso8601(config['start'])
    end = parse_iso8601(config['end'])
    start_ms = int(start.timestamp() * 1000)
    end_ms = int(end.timestamp() * 1000)

    # Analyze each table
    for table in config['tables']:
        analysis = analyze_continuity(
            client=client,
            table=table,
            symbol=config['symbol'],
            start_ms=start_ms,
            end_ms=end_ms,
            max_gap_sec=60  # 1 minute threshold
        )

        recommendations = get_cleaning_recommendations(analysis)
        print_analysis_report(analysis, recommendations)
        print("\n")


if __name__ == "__main__":
    main()
