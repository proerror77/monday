use std::collections::{HashMap, HashSet};

use polymarket_client_sdk::gamma::types::request::{MarketsRequest, TeamsRequest};
use polymarket_client_sdk::gamma::types::response::{Event, Market, Team};
use polymarket_client_sdk::gamma::Client as GammaClient;
use serde_json::Value;

use crate::discovery::types::{MarketDescriptor, MarketFamily, MarketSemantics, SettlementSource};
use crate::gamma_keyset::fetch_markets;

#[derive(Debug, Clone)]
pub struct DiscoveredSportsMarket {
    pub descriptor: MarketDescriptor,
    pub raw_event: Option<Value>,
    pub raw_market: Value,
}

pub async fn discover_sports_markets(
    client: &GammaClient,
    limit: i32,
) -> polymarket_client_sdk::Result<Vec<DiscoveredSportsMarket>> {
    let sports = client.sports().await?;
    let sports_market_types = client.sports_market_types().await?;
    let teams = client.teams(&TeamsRequest::default()).await?;

    let leagues: HashSet<String> = teams
        .iter()
        .filter_map(|team| team.league.clone())
        .collect();

    let mut request = MarketsRequest::default();
    request.closed = Some(false);
    request.limit = Some(limit.min(100));
    request.sports_market_types = sports_market_types.market_types;

    let markets = fetch_markets(&request, limit.max(1) as usize).await?;
    Ok(normalize_sports_markets(
        &markets, &teams, &sports, &leagues,
    ))
}

fn normalize_sports_markets(
    markets: &[Market],
    teams: &[Team],
    sports: &[polymarket_client_sdk::gamma::types::response::SportsMetadata],
    leagues: &HashSet<String>,
) -> Vec<DiscoveredSportsMarket> {
    let team_map: HashMap<String, &Team> = teams
        .iter()
        .map(|team| (team.id.to_string(), team))
        .collect();
    let sport_codes: HashSet<String> = sports
        .iter()
        .map(|sport| sport.sport.to_ascii_lowercase())
        .collect();

    let mut discovered = Vec::new();

    for market in markets {
        if let Some(item) = normalize_sports_market(market, &team_map, &sport_codes, leagues) {
            discovered.push(item);
        }
    }

    discovered
}

fn normalize_sports_market(
    market: &Market,
    team_map: &HashMap<String, &Team>,
    sport_codes: &HashSet<String>,
    leagues: &HashSet<String>,
) -> Option<DiscoveredSportsMarket> {
    if market.sports_market_type.is_none() && market.game_id.is_none() {
        return None;
    }
    if matches!(market.active, Some(false)) {
        return None;
    }

    let event = market.events.as_ref().and_then(|events| events.first());
    let team_a = market.team_a_id.as_ref().and_then(|id| team_map.get(id));
    let team_b = market.team_b_id.as_ref().and_then(|id| team_map.get(id));

    let league = team_a
        .and_then(|team| team.league.clone())
        .or_else(|| team_b.and_then(|team| team.league.clone()))
        .or_else(|| event.and_then(|value| value.subcategory.clone()))
        .or_else(|| market.subcategory.clone())
        .or_else(|| event.and_then(|value| value.category.clone()))
        .or_else(|| market.category.clone());

    let sport = event
        .and_then(|value| value.subcategory.clone())
        .or_else(|| {
            league.as_ref().and_then(|value| {
                let normalized = value.to_ascii_lowercase();
                sport_codes.contains(&normalized).then(|| value.clone())
            })
        })
        .or_else(|| event.and_then(|value| value.category.clone()))
        .or_else(|| market.category.clone());

    let home_team = event
        .and_then(|value| value.home_team_name.clone())
        .or_else(|| team_a.and_then(|team| team.name.clone()));
    let away_team = event
        .and_then(|value| value.away_team_name.clone())
        .or_else(|| team_b.and_then(|team| team.name.clone()));

    if let Some(found_league) = &league {
        if !leagues.is_empty() && !leagues.contains(found_league) {
            // Still allow descriptors through even if the league set is incomplete.
        }
    }

    let descriptor = MarketDescriptor {
        market_family: MarketFamily::Sports,
        event_id: event
            .map(|value| value.id.clone())
            .or_else(|| market.game_id.clone()),
        event_slug: event.and_then(|value| value.slug.clone()),
        market_id: market.id.clone(),
        market_slug: market.slug.clone(),
        title: market
            .question
            .clone()
            .or_else(|| event.and_then(|value| value.title.clone())),
        strategy_symbol: None,
        reference_symbol: event
            .and_then(|value| value.sportsradar_match_id.clone())
            .or_else(|| market.game_id.clone()),
        settlement_source: SettlementSource::OfficialPolymarket,
        league,
        sport,
        start_time: market
            .event_start_time
            .or_else(|| event.and_then(|value| value.start_time)),
        end_time: market
            .end_date
            .or_else(|| event.and_then(|value| value.end_date)),
        token_ids: market
            .clob_token_ids
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|token| token.to_string())
            .collect(),
        market_semantics: MarketSemantics::from_sports_market_type(
            market.sports_market_type.as_deref(),
        ),
        home_team,
        away_team,
        active: market.active,
        accepting_orders: market.accepting_orders,
    };

    Some(DiscoveredSportsMarket {
        descriptor,
        raw_event: event.and_then(event_to_value),
        raw_market: serde_json::to_value(market).ok()?,
    })
}

fn event_to_value(event: &Event) -> Option<Value> {
    serde_json::to_value(event).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use serde_json::json;

    use super::normalize_sports_market;
    use crate::discovery::types::MarketSemantics;

    #[test]
    fn normalizes_sports_market_with_league_and_team_metadata() {
        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "sports-market-1",
                "question": "Will the Lakers beat the Celtics?",
                "slug": "lakers-vs-celtics-moneyline",
                "sportsMarketType": "moneyline",
                "gameId": "game-123",
                "teamAID": "1",
                "teamBID": "2",
                "eventStartTime": "2026-04-06T01:00:00Z",
                "endDate": "2026-04-06T03:30:00Z",
                "clobTokenIds": "[\"11\",\"22\"]",
                "active": true
            }))
            .unwrap();

        let team_a: polymarket_client_sdk::gamma::types::response::Team =
            serde_json::from_value(json!({
                "id": 1,
                "name": "Lakers",
                "league": "NBA"
            }))
            .unwrap();
        let team_b: polymarket_client_sdk::gamma::types::response::Team =
            serde_json::from_value(json!({
                "id": 2,
                "name": "Celtics",
                "league": "NBA"
            }))
            .unwrap();

        let mut team_map = HashMap::new();
        team_map.insert("1".to_string(), &team_a);
        team_map.insert("2".to_string(), &team_b);
        let sport_codes = HashSet::from(["nba".to_string()]);
        let leagues = HashSet::from(["NBA".to_string()]);

        let item = normalize_sports_market(&market, &team_map, &sport_codes, &leagues)
            .expect("market should normalize");

        assert_eq!(item.descriptor.market_family.as_str(), "sports");
        assert_eq!(item.descriptor.market_semantics, MarketSemantics::Moneyline);
        assert_eq!(item.descriptor.league.as_deref(), Some("NBA"));
        assert_eq!(item.descriptor.home_team.as_deref(), Some("Lakers"));
        assert_eq!(item.descriptor.away_team.as_deref(), Some("Celtics"));
        assert_eq!(
            item.descriptor.reference_symbol.as_deref(),
            Some("game-123")
        );
        assert_eq!(item.descriptor.token_ids, vec!["11", "22"]);
    }
}
