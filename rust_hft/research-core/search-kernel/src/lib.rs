//! Domain-neutral deterministic UCT tree mechanics.
//!
//! Candidate payloads, expansion grammars, evaluators, and persistence remain
//! owned by domain adapters. This crate owns deterministic choice, UCT
//! selection, tree invariants, and reward-statistic updates.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicRng(u64);

impl DeterministicRng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn index(&mut self, len: usize) -> usize {
        assert!(len > 0, "deterministic RNG requires a non-empty range");
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 as usize) % len
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UctStats {
    visits: u64,
    total_reward: f64,
    best_reward: Option<f64>,
}

impl Default for UctStats {
    fn default() -> Self {
        Self {
            visits: 0,
            total_reward: 0.0,
            best_reward: None,
        }
    }
}

impl UctStats {
    pub fn from_parts(
        visits: u64,
        total_reward: f64,
        best_reward: Option<f64>,
    ) -> Result<Self, UctError> {
        let stats = Self {
            visits,
            total_reward,
            best_reward,
        };
        stats.validate()?;
        Ok(stats)
    }

    pub fn visits(self) -> u64 {
        self.visits
    }

    pub fn total_reward(self) -> f64 {
        self.total_reward
    }

    pub fn best_reward(self) -> Option<f64> {
        self.best_reward
    }

    fn validate(self) -> Result<(), UctError> {
        if !self.total_reward.is_finite()
            || self.best_reward.is_some_and(|reward| !reward.is_finite())
            || (self.visits == 0) != (self.total_reward == 0.0 && self.best_reward.is_none())
        {
            return Err(UctError::InvalidStats);
        }
        Ok(())
    }

    fn record(self, reward: f64) -> Result<Self, UctError> {
        self.validate()?;
        if !reward.is_finite() {
            return Err(UctError::InvalidReward);
        }
        let visits = self.visits.checked_add(1).ok_or(UctError::StatsOverflow)?;
        let total_reward = self.total_reward + reward;
        if !total_reward.is_finite() {
            return Err(UctError::StatsOverflow);
        }
        Ok(Self {
            visits,
            total_reward,
            best_reward: Some(self.best_reward.map_or(reward, |best| best.max(reward))),
        })
    }
}

/// The tree facts required by UCT. Domain adapters may keep the three statistic
/// fields flat in an existing wire format; their update semantics live here.
pub trait UctNode {
    fn parent(&self) -> Option<usize>;
    fn children(&self) -> &[usize];
    fn is_expandable(&self) -> bool;
    fn depth(&self) -> usize;
    fn stats(&self) -> Result<UctStats, UctError>;
    fn replace_stats(&mut self, stats: UctStats);
    fn subtree_is_expandable(&self) -> bool {
        self.is_expandable()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UctError {
    InvalidExploration,
    InvalidNode(usize),
    InvalidReward,
    InvalidStats,
    StatsOverflow,
    InvalidTopology(usize),
    CyclicTree,
}

impl fmt::Display for UctError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidExploration => {
                formatter.write_str("UCT exploration must be finite and non-negative")
            }
            Self::InvalidNode(node_id) => {
                write!(formatter, "UCT tree references missing node {node_id}")
            }
            Self::InvalidReward => formatter.write_str("UCT reward must be finite"),
            Self::InvalidStats => formatter.write_str("UCT reward statistics are invalid"),
            Self::StatsOverflow => formatter.write_str("UCT reward statistics overflowed"),
            Self::InvalidTopology(node_id) => {
                write!(formatter, "UCT tree topology is invalid at node {node_id}")
            }
            Self::CyclicTree => formatter.write_str("UCT tree contains a cycle"),
        }
    }
}

impl std::error::Error for UctError {}

pub fn validate_tree<N: UctNode>(
    nodes: &[N],
    root: usize,
    max_depth: usize,
) -> Result<(), UctError> {
    let root_node = nodes.get(root).ok_or(UctError::InvalidNode(root))?;
    if root_node.parent().is_some() || root_node.depth() != 0 {
        return Err(UctError::InvalidTopology(root));
    }

    for (node_id, node) in nodes.iter().enumerate() {
        node.stats()?;
        if node.depth() > max_depth || node.children().windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(UctError::InvalidTopology(node_id));
        }
        for &child_id in node.children() {
            if child_id <= node_id {
                return Err(UctError::CyclicTree);
            }
            let child = nodes.get(child_id).ok_or(UctError::InvalidNode(child_id))?;
            if child.parent() != Some(node_id) || child.depth() != node.depth() + 1 {
                return Err(UctError::InvalidTopology(node_id));
            }
        }

        if node_id == root {
            continue;
        }
        let parent_id = node.parent().ok_or(UctError::InvalidTopology(node_id))?;
        if parent_id >= node_id {
            return Err(UctError::CyclicTree);
        }
        let parent = nodes
            .get(parent_id)
            .ok_or(UctError::InvalidNode(parent_id))?;
        if !parent.children().contains(&node_id) || node.depth() != parent.depth() + 1 {
            return Err(UctError::InvalidTopology(node_id));
        }
    }
    Ok(())
}

pub fn select_expandable<N: UctNode>(
    nodes: &[N],
    root: usize,
    exploration: f64,
) -> Result<Option<usize>, UctError> {
    if !exploration.is_finite() || exploration < 0.0 {
        return Err(UctError::InvalidExploration);
    }
    validate_tree(nodes, root, usize::MAX)?;
    select_from(nodes, root, exploration)
}

/// Selects with deterministic square-root progressive widening, falling back
/// to a node's remaining action only after its eligible descendants are spent.
pub fn select_expandable_progressively<N: UctNode>(
    nodes: &[N],
    root: usize,
    exploration: f64,
) -> Result<Option<usize>, UctError> {
    if !exploration.is_finite() || exploration < 0.0 {
        return Err(UctError::InvalidExploration);
    }
    select_progressively(nodes, root, exploration)
}

fn select_from<N: UctNode>(
    nodes: &[N],
    node_id: usize,
    exploration: f64,
) -> Result<Option<usize>, UctError> {
    let node = nodes.get(node_id).ok_or(UctError::InvalidNode(node_id))?;
    if node.is_expandable() {
        return Ok(Some(node_id));
    }

    let parent_visits = node.stats()?.visits();
    let mut best = None;
    for &child_id in node.children() {
        if let Some(expandable_id) = select_from(nodes, child_id, exploration)? {
            let score = uct_score(nodes, child_id, parent_visits, exploration)?;
            if best.is_none_or(|(_, best_score)| {
                score.total_cmp(&best_score) != std::cmp::Ordering::Less
            }) {
                best = Some((expandable_id, score));
            }
        }
    }
    Ok(best.map(|(expandable_id, _)| expandable_id))
}

fn select_progressively<N: UctNode>(
    nodes: &[N],
    root: usize,
    exploration: f64,
) -> Result<Option<usize>, UctError> {
    let root_node = nodes.get(root).ok_or(UctError::InvalidNode(root))?;
    if root_node.parent().is_some() || root_node.depth() != 0 {
        return Err(UctError::InvalidTopology(root));
    }
    if !root_node.subtree_is_expandable() {
        return Ok(None);
    }

    let mut node_id = root;
    loop {
        let node = nodes.get(node_id).ok_or(UctError::InvalidNode(node_id))?;
        let stats = node.stats()?;
        let expandable = node.is_expandable();
        let next_child = u64::try_from(node.children().len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        if expandable && next_child.saturating_mul(next_child) <= stats.visits().saturating_add(1) {
            return Ok(Some(node_id));
        }

        let parent_visits = stats.visits();
        // ponytail: scan siblings on the chosen path; cache scores only if measured branching dominates.
        let mut best = None;
        for &child_id in node.children() {
            let child = nodes.get(child_id).ok_or(UctError::InvalidNode(child_id))?;
            if child_id <= node_id
                || child.parent() != Some(node_id)
                || child.depth() != node.depth() + 1
            {
                return Err(UctError::InvalidTopology(node_id));
            }
            if !child.subtree_is_expandable() {
                continue;
            }
            let score = uct_score(nodes, child_id, parent_visits, exploration)?;
            if best.is_none_or(|(_, best_score)| {
                score.total_cmp(&best_score) != std::cmp::Ordering::Less
            }) {
                best = Some((child_id, score));
            }
        }
        if let Some((child_id, _)) = best {
            node_id = child_id;
            continue;
        }
        if expandable {
            return Ok(Some(node_id));
        }
        return Err(UctError::InvalidTopology(node_id));
    }
}

fn uct_score<N: UctNode>(
    nodes: &[N],
    node_id: usize,
    parent_visits: u64,
    exploration: f64,
) -> Result<f64, UctError> {
    let stats = nodes
        .get(node_id)
        .ok_or(UctError::InvalidNode(node_id))?
        .stats()?;
    if stats.visits() == 0 {
        return Ok(f64::INFINITY);
    }
    Ok(stats.total_reward() / stats.visits() as f64
        + exploration * ((parent_visits.max(1) as f64).ln() / stats.visits() as f64).sqrt())
}

pub fn backpropagate<N: UctNode>(
    nodes: &mut [N],
    root: usize,
    leaf: usize,
    reward: f64,
) -> Result<(), UctError> {
    if !reward.is_finite() {
        return Err(UctError::InvalidReward);
    }
    validate_tree(nodes, root, usize::MAX)?;

    let mut lineage = Vec::new();
    let mut current = leaf;
    loop {
        let node = nodes.get(current).ok_or(UctError::InvalidNode(current))?;
        lineage.push(current);
        if current == root {
            break;
        }
        current = node.parent().ok_or(UctError::InvalidTopology(current))?;
    }

    let updated = lineage
        .iter()
        .map(|&node_id| nodes[node_id].stats()?.record(reward))
        .collect::<Result<Vec<_>, UctError>>()?;
    for (node_id, stats) in lineage.into_iter().zip(updated) {
        nodes[node_id].replace_stats(stats);
    }
    Ok(())
}

/// Updates only the selected lineage after the adapter has validated the full
/// append-only tree at its checkpoint boundary.
pub fn backpropagate_lineage<N: UctNode>(
    nodes: &mut [N],
    root: usize,
    leaf: usize,
    reward: f64,
) -> Result<(), UctError> {
    if !reward.is_finite() {
        return Err(UctError::InvalidReward);
    }
    let root_node = nodes.get(root).ok_or(UctError::InvalidNode(root))?;
    if root_node.parent().is_some() || root_node.depth() != 0 {
        return Err(UctError::InvalidTopology(root));
    }

    let mut lineage = Vec::new();
    let mut current = leaf;
    loop {
        let node = nodes.get(current).ok_or(UctError::InvalidNode(current))?;
        lineage.push(current);
        if current == root {
            break;
        }
        let parent_id = node.parent().ok_or(UctError::InvalidTopology(current))?;
        let parent = nodes
            .get(parent_id)
            .ok_or(UctError::InvalidNode(parent_id))?;
        if parent_id >= current
            || !parent.children().contains(&current)
            || node.depth() != parent.depth() + 1
        {
            return Err(UctError::InvalidTopology(current));
        }
        current = parent_id;
    }

    let updated = lineage
        .iter()
        .map(|&node_id| nodes[node_id].stats()?.record(reward))
        .collect::<Result<Vec<_>, UctError>>()?;
    for (node_id, stats) in lineage.into_iter().zip(updated) {
        nodes[node_id].replace_stats(stats);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Node {
        parent: Option<usize>,
        children: Vec<usize>,
        expandable: bool,
        subtree_expandable: bool,
        depth: usize,
        stats: UctStats,
    }

    impl UctNode for Node {
        fn parent(&self) -> Option<usize> {
            self.parent
        }
        fn children(&self) -> &[usize] {
            &self.children
        }
        fn is_expandable(&self) -> bool {
            self.expandable
        }
        fn depth(&self) -> usize {
            self.depth
        }
        fn stats(&self) -> Result<UctStats, UctError> {
            Ok(self.stats)
        }
        fn replace_stats(&mut self, stats: UctStats) {
            self.stats = stats;
        }
        fn subtree_is_expandable(&self) -> bool {
            self.subtree_expandable
        }
    }

    #[test]
    fn deterministic_rng_preserves_the_existing_sequence() {
        let mut rng = DeterministicRng::new(3);
        assert_eq!(rng.index(4), 2);
    }

    #[test]
    fn selects_and_backpropagates_without_domain_knowledge() {
        let mut nodes = vec![
            Node {
                parent: None,
                children: vec![1],
                expandable: false,
                subtree_expandable: true,
                depth: 0,
                stats: UctStats::from_parts(1, 0.2, Some(0.2)).unwrap(),
            },
            Node {
                parent: Some(0),
                children: vec![],
                expandable: true,
                subtree_expandable: true,
                depth: 1,
                stats: UctStats::default(),
            },
        ];
        assert_eq!(select_expandable(&nodes, 0, 1.4).unwrap(), Some(1));
        backpropagate(&mut nodes, 0, 1, 0.5).unwrap();
        assert_eq!(
            nodes[0].stats,
            UctStats::from_parts(2, 0.7, Some(0.5)).unwrap()
        );
        assert_eq!(
            nodes[1].stats,
            UctStats::from_parts(1, 0.5, Some(0.5)).unwrap()
        );
    }

    #[test]
    fn selection_preserves_total_order_for_signed_zero() {
        let nodes = vec![
            Node {
                parent: None,
                children: vec![1, 2],
                expandable: false,
                subtree_expandable: true,
                depth: 0,
                stats: UctStats::from_parts(2, 0.0, Some(0.0)).unwrap(),
            },
            Node {
                parent: Some(0),
                children: vec![],
                expandable: true,
                subtree_expandable: true,
                depth: 1,
                stats: UctStats::from_parts(1, 0.0, Some(0.0)).unwrap(),
            },
            Node {
                parent: Some(0),
                children: vec![],
                expandable: true,
                subtree_expandable: true,
                depth: 1,
                stats: UctStats::from_parts(1, -0.0, Some(-0.0)).unwrap(),
            },
        ];

        assert_eq!(select_expandable(&nodes, 0, -0.0).unwrap(), Some(1));
    }

    #[test]
    fn progressive_selection_descends_before_exhausting_a_parent() {
        let nodes = vec![
            Node {
                parent: None,
                children: vec![1],
                expandable: true,
                subtree_expandable: true,
                depth: 0,
                stats: UctStats::from_parts(2, 0.4, Some(0.2)).unwrap(),
            },
            Node {
                parent: Some(0),
                children: vec![],
                expandable: true,
                subtree_expandable: true,
                depth: 1,
                stats: UctStats::from_parts(1, 0.2, Some(0.2)).unwrap(),
            },
        ];

        assert_eq!(select_expandable(&nodes, 0, 1.4).unwrap(), Some(0));
        assert_eq!(
            select_expandable_progressively(&nodes, 0, 1.4).unwrap(),
            Some(1)
        );

        let mut exhausted_child = nodes;
        exhausted_child[1].expandable = false;
        exhausted_child[1].subtree_expandable = false;
        assert_eq!(
            select_expandable_progressively(&exhausted_child, 0, 1.4).unwrap(),
            Some(0)
        );
    }

    #[test]
    fn progressive_selection_handles_a_deep_tree_without_recursion() {
        let depth = 10_000;
        let nodes = (0..depth)
            .map(|node_id| Node {
                parent: (node_id > 0).then(|| node_id - 1),
                children: (node_id + 1 < depth)
                    .then(|| vec![node_id + 1])
                    .unwrap_or_default(),
                expandable: node_id + 1 == depth,
                subtree_expandable: true,
                depth: node_id,
                stats: UctStats::default(),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            select_expandable_progressively(&nodes, 0, 1.4).unwrap(),
            Some(depth - 1)
        );
    }

    #[test]
    fn progressive_path_operations_skip_unselected_subtrees() {
        let invalid_stats = UctStats {
            visits: 1,
            total_reward: f64::NAN,
            best_reward: Some(f64::NAN),
        };
        let mut nodes = vec![
            Node {
                parent: None,
                children: vec![1, 2],
                expandable: false,
                subtree_expandable: true,
                depth: 0,
                stats: UctStats::from_parts(2, 2.0, Some(2.0)).unwrap(),
            },
            Node {
                parent: Some(0),
                children: vec![3],
                expandable: false,
                subtree_expandable: true,
                depth: 1,
                stats: UctStats::from_parts(1, 2.0, Some(2.0)).unwrap(),
            },
            Node {
                parent: Some(0),
                children: vec![4],
                expandable: false,
                subtree_expandable: true,
                depth: 1,
                stats: UctStats::from_parts(1, 0.0, Some(0.0)).unwrap(),
            },
            Node {
                parent: Some(1),
                children: vec![],
                expandable: true,
                subtree_expandable: true,
                depth: 2,
                stats: UctStats::default(),
            },
            Node {
                parent: Some(2),
                children: vec![],
                expandable: true,
                subtree_expandable: true,
                depth: 2,
                stats: invalid_stats,
            },
        ];

        assert_eq!(
            select_expandable_progressively(&nodes, 0, 1.4).unwrap(),
            Some(3)
        );
        backpropagate_lineage(&mut nodes, 0, 3, 0.5).unwrap();
        assert_eq!(nodes[3].stats.visits(), 1);
        assert_eq!(nodes[4].stats.visits(), invalid_stats.visits());
        assert!(nodes[4].stats.total_reward().is_nan());
    }

    #[test]
    fn rejects_an_expandable_cycle_before_mutating_rewards() {
        let mut nodes = vec![
            Node {
                parent: None,
                children: vec![1],
                expandable: false,
                subtree_expandable: true,
                depth: 0,
                stats: UctStats::default(),
            },
            Node {
                parent: Some(0),
                children: vec![2],
                expandable: true,
                subtree_expandable: true,
                depth: 1,
                stats: UctStats::default(),
            },
            Node {
                parent: Some(1),
                children: vec![1],
                expandable: false,
                subtree_expandable: false,
                depth: 2,
                stats: UctStats::default(),
            },
        ];
        assert_eq!(
            backpropagate(&mut nodes, 0, 2, 0.5),
            Err(UctError::CyclicTree)
        );
        assert!(nodes.iter().all(|node| node.stats == UctStats::default()));
        assert_eq!(select_expandable(&nodes, 0, 1.4), Err(UctError::CyclicTree));
    }

    #[test]
    fn rejects_reward_overflow_without_partial_mutation() {
        let original = UctStats::from_parts(1, f64::MAX, Some(f64::MAX)).unwrap();
        let mut nodes = vec![Node {
            parent: None,
            children: vec![],
            expandable: true,
            subtree_expandable: true,
            depth: 0,
            stats: original,
        }];

        assert_eq!(
            backpropagate(&mut nodes, 0, 0, f64::MAX),
            Err(UctError::StatsOverflow)
        );
        assert_eq!(nodes[0].stats, original);
    }
}
