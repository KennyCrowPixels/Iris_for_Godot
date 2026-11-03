// Dataweb v2 — a brain-like 2D associative memory.
//
// Goals (from the product spec): a sustainable, 2-dimensional store of knowledge that behaves
// like neural tissue. Similar entries are placed near each other, connected by weighted "synapse"
// links, and retrieval walks those links like neural pathways (spreading activation) instead of
// scanning the whole store. This keeps recall bounded and fast even when the web holds a very large
// number of entries, because a query only lights up a local neighborhood rather than every node.
//
// The engine here is pure and deterministic (no clock, no I/O) so it is fully unit-testable. The
// Tauri command layer in commands.rs persists it as JSON and supplies timestamps.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Maximum entries kept in a single web before the least-valuable nodes are pruned (synaptic
/// pruning). Chosen so an O(n) insert stays comfortably fast on a local machine.
pub const MAX_NODES: usize = 6000;
/// How many strongest neighbors (synapses) each node keeps.
pub const MAX_NEIGHBORS: usize = 6;
/// Minimum similarity for two nodes to be linked at all.
const LINK_THRESHOLD: f64 = 0.08;

fn is_stopword(token: &str) -> bool {
    matches!(
        token,
        "the" | "and" | "for" | "are" | "but" | "not" | "you" | "with" | "that" | "this" | "have"
            | "from" | "they" | "was" | "were" | "has" | "had" | "its" | "our" | "your" | "their"
            | "what" | "when" | "where" | "which" | "who" | "whom" | "how" | "why" | "can" | "will"
            | "would" | "could" | "should" | "about" | "into" | "than" | "then" | "them" | "there"
            | "here" | "been" | "being" | "does" | "did" | "done" | "also" | "just" | "like" | "some"
            | "any" | "all" | "one" | "two" | "get" | "got" | "use" | "used" | "using" | "it's"
    )
}

/// Lowercase alphanumeric tokens, stopwords removed, deduped, order-preserving, capped.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut tokens = Vec::new();
    for raw in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        let token = raw.trim().to_ascii_lowercase();
        if token.len() < 3 || is_stopword(&token) {
            continue;
        }
        if seen.insert(token.clone()) {
            tokens.push(token);
        }
        if tokens.len() >= 48 {
            break;
        }
    }
    tokens
}

/// Weighted token overlap in [0,1]. Rarer tokens (counted via `idf`) carry more weight so a shared
/// distinctive term ("sphere", "greenhouse") matters more than a shared common one.
fn weighted_similarity(a: &[String], b: &[String], idf: &HashMap<String, f64>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let bset: HashSet<&String> = b.iter().collect();
    let mut shared = 0.0;
    let mut total = 0.0;
    let mut counted: HashSet<&String> = HashSet::new();
    for token in a.iter().chain(b.iter()) {
        if !counted.insert(token) {
            continue;
        }
        let weight = idf.get(token).copied().unwrap_or(1.0);
        total += weight;
        if a.contains(token) && bset.contains(token) {
            shared += weight;
        }
    }
    if total <= 0.0 {
        0.0
    } else {
        shared / total
    }
}

/// Fast unweighted Jaccard overlap, used on the insert hot-path (no per-insert IDF rebuild) so
/// building a large web stays close to linear overall.
fn jaccard(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let bset: HashSet<&String> = b.iter().collect();
    let shared = a.iter().filter(|token| bset.contains(token)).count();
    let union = a.len() + b.len() - shared;
    if union == 0 {
        0.0
    } else {
        shared as f64 / union as f64
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryNode {
    pub id: u64,
    pub content: String,
    pub keywords: Vec<String>,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub neighbors: Vec<u64>,
    #[serde(default)]
    pub hits: u64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataWeb {
    pub nodes: Vec<MemoryNode>,
    #[serde(default)]
    pub next_id: u64,
}

/// Deterministic small hash used for stable jitter/placement (no RNG, so tests are reproducible).
fn stable_hash(seed: u64) -> f64 {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    x ^= x >> 33;
    (x % 100_000) as f64 / 100_000.0
}

impl DataWeb {
    fn index_of(&self, id: u64) -> Option<usize> {
        self.nodes.iter().position(|node| node.id == id)
    }

    /// Inverse-document-frequency per token across the web, so distinctive terms dominate matching.
    fn idf(&self) -> HashMap<String, f64> {
        let total = self.nodes.len().max(1) as f64;
        let mut df: HashMap<String, u64> = HashMap::new();
        for node in &self.nodes {
            for token in &node.keywords {
                *df.entry(token.clone()).or_default() += 1;
            }
        }
        df.into_iter()
            .map(|(token, count)| (token, (total / count as f64).ln() + 1.0))
            .collect()
    }

    /// Store a new memory. Places it spatially near its most similar existing node (so like clusters
    /// with like), links its strongest neighbors bidirectionally, and prunes if over capacity.
    /// Near-duplicate content reinforces the existing node instead of creating a new one.
    pub fn remember(&mut self, content: &str, now: i64) -> u64 {
        let content = content.trim();
        let keywords = tokenize(content);

        // Reinforce an (almost) identical existing memory rather than duplicating it.
        if let Some(existing) = self.nodes.iter_mut().find(|node| {
            node.content.eq_ignore_ascii_case(content)
        }) {
            existing.hits += 1;
            existing.updated_at = now;
            return existing.id;
        }

        let id = self.next_id.max(self.nodes.iter().map(|n| n.id + 1).max().unwrap_or(1));
        self.next_id = id + 1;

        // Similarity to every current node (O(n); fast unweighted overlap, bounded by MAX_NODES).
        let mut scored: Vec<(usize, f64)> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (index, jaccard(&keywords, &node.keywords)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Place near the most similar node; distance grows as similarity falls. Nodes with nothing
        // similar are pushed far out so unrelated memories never land in a dense cluster.
        let (x, y) = match scored.first().filter(|(_, sim)| *sim > 0.0) {
            Some(&(index, sim)) => {
                let anchor = &self.nodes[index];
                let angle = stable_hash(id) * std::f64::consts::TAU;
                let radius = 0.25 + (1.0 - sim) * 1.5;
                (anchor.x + radius * angle.cos(), anchor.y + radius * angle.sin())
            }
            None => {
                let angle = stable_hash(id) * std::f64::consts::TAU;
                let radius = 8.0 + self.nodes.len() as f64 * 0.25;
                (radius * angle.cos(), radius * angle.sin())
            }
        };

        let neighbors: Vec<u64> = scored
            .iter()
            .filter(|(_, sim)| *sim >= LINK_THRESHOLD)
            .take(MAX_NEIGHBORS)
            .map(|(index, _)| self.nodes[*index].id)
            .collect();

        // Link bidirectionally; keep each existing node's neighbor list bounded and strongest-first.
        for &neighbor_id in &neighbors {
            if let Some(index) = self.index_of(neighbor_id) {
                if !self.nodes[index].neighbors.contains(&id) {
                    self.nodes[index].neighbors.push(id);
                    if self.nodes[index].neighbors.len() > MAX_NEIGHBORS {
                        let node_keywords = self.nodes[index].keywords.clone();
                        self.rebuild_neighbor_order(index, &node_keywords);
                    }
                }
            }
        }

        self.nodes.push(MemoryNode {
            id,
            content: content.to_string(),
            keywords,
            x,
            y,
            neighbors,
            hits: 0,
            created_at: now,
            updated_at: now,
        });

        self.prune(now);
        id
    }

    fn rebuild_neighbor_order(&mut self, index: usize, keywords: &[String]) {
        let mut ranked: Vec<(u64, f64)> = self.nodes[index]
            .neighbors
            .iter()
            .filter_map(|&nid| {
                self.index_of(nid)
                    .map(|j| (nid, jaccard(keywords, &self.nodes[j].keywords)))
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(MAX_NEIGHBORS);
        self.nodes[index].neighbors = ranked.into_iter().map(|(nid, _)| nid).collect();
    }

    /// Spreading-activation recall: find a few entry nodes by direct relevance, then walk the
    /// synapse graph outward with decaying activation, visiting a bounded number of nodes. Returns
    /// node ids ranked by accumulated activation — direct hits and their strong associations.
    pub fn recall_ids(&self, query: &str, limit: usize, now: i64) -> Vec<u64> {
        if self.nodes.is_empty() {
            return Vec::new();
        }
        let query_tokens = tokenize(query);
        if query_tokens.is_empty() {
            // No usable query terms: surface the most reinforced/recent memories.
            let mut ranked: Vec<&MemoryNode> = self.nodes.iter().collect();
            ranked.sort_by(|a, b| {
                self.base_value(b, now)
                    .partial_cmp(&self.base_value(a, now))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            return ranked.into_iter().take(limit).map(|n| n.id).collect();
        }
        let idf = self.idf();

        // Seed activation from direct relevance.
        let mut seeds: Vec<(u64, f64)> = self
            .nodes
            .iter()
            .filter_map(|node| {
                let sim = weighted_similarity(&query_tokens, &node.keywords, &idf);
                (sim > 0.0).then_some((node.id, sim + 0.02 * self.base_value(node, now)))
            })
            .collect();
        seeds.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        seeds.truncate(6);

        // Breadth-first spread with per-hop decay; bounded visit budget keeps recall cheap at scale.
        let mut activation: HashMap<u64, f64> = HashMap::new();
        let mut frontier: Vec<(u64, f64, u32)> = seeds
            .into_iter()
            .map(|(id, score)| (id, score, 0u32))
            .collect();
        for &(id, score, _) in &frontier {
            activation.insert(id, score);
        }
        let mut visited = 0usize;
        while let Some((id, energy, depth)) = frontier.pop() {
            visited += 1;
            if visited > 80 || depth >= 3 || energy < 0.02 {
                continue;
            }
            if let Some(index) = self.index_of(id) {
                for &neighbor_id in &self.nodes[index].neighbors {
                    if let Some(j) = self.index_of(neighbor_id) {
                        let edge = weighted_similarity(&self.nodes[index].keywords, &self.nodes[j].keywords, &idf);
                        let passed = energy * (0.35 + 0.35 * edge);
                        let entry = activation.entry(neighbor_id).or_insert(0.0);
                        if passed > *entry {
                            *entry = passed;
                            frontier.push((neighbor_id, passed, depth + 1));
                        }
                    }
                }
            }
        }

        let mut ranked: Vec<(u64, f64)> = activation.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.into_iter().take(limit).map(|(id, _)| id).collect()
    }

    /// Recall and return the matching nodes (ordered). Marks them as accessed (reinforcement) when
    /// `reinforce` is set, strengthening frequently-walked pathways.
    pub fn recall(&mut self, query: &str, limit: usize, now: i64, reinforce: bool) -> Vec<MemoryNode> {
        let ids = self.recall_ids(query, limit, now);
        let mut out = Vec::new();
        for id in ids {
            if let Some(index) = self.index_of(id) {
                if reinforce {
                    self.nodes[index].hits += 1;
                    self.nodes[index].updated_at = now;
                }
                out.push(self.nodes[index].clone());
            }
        }
        out
    }

    /// Node value for ranking/pruning: reinforcement (hits) plus recency decay.
    fn base_value(&self, node: &MemoryNode, now: i64) -> f64 {
        let age_days = ((now - node.updated_at).max(0) as f64) / 86_400.0;
        let recency = (-age_days / 45.0).exp(); // ~45-day half-life-ish decay
        (node.hits as f64) + recency + 0.1 * node.neighbors.len() as f64
    }

    /// Synaptic pruning: when over capacity, drop the least-valuable nodes and unlink them.
    fn prune(&mut self, now: i64) {
        if self.nodes.len() <= MAX_NODES {
            return;
        }
        let mut valued: Vec<(usize, f64)> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (index, self.base_value(node, now)))
            .collect();
        valued.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        let remove: HashSet<u64> = valued
            .into_iter()
            .take(self.nodes.len() - MAX_NODES)
            .map(|(index, _)| self.nodes[index].id)
            .collect();
        self.nodes.retain(|node| !remove.contains(&node.id));
        for node in &mut self.nodes {
            node.neighbors.retain(|id| !remove.contains(id));
        }
    }

    /// Compact, bounded digest for injecting relevant memory into a model's context. Only the
    /// recalled neighborhood is included, so this stays small no matter how large the web grows.
    pub fn recall_digest(&self, query: &str, max_chars: usize, now: i64) -> String {
        let ids = self.recall_ids(query, 8, now);
        if ids.is_empty() {
            return String::new();
        }
        let mut out = String::from("Relevant stored memory (associative recall; verify before acting):\n");
        for id in ids {
            if let Some(node) = self.nodes.iter().find(|n| n.id == id) {
                let line = format!("- {}\n", node.content.replace('\n', " "));
                if out.len() + line.len() > max_chars {
                    break;
                }
                out.push_str(&line);
            }
        }
        out
    }

    pub fn stats(&self) -> Value {
        let edges: usize = self.nodes.iter().map(|n| n.neighbors.len()).sum::<usize>() / 2;
        let total_hits: u64 = self.nodes.iter().map(|n| n.hits).sum();
        json!({
            "nodes": self.nodes.len(),
            "edges": edges,
            "totalRecalls": total_hits,
            "capacity": MAX_NODES,
            "meaning": "Associative 2D memory: entries cluster by similarity and recall walks synapse links. This is retrieval support, not verified fact or model training.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> DataWeb {
        let mut web = DataWeb::default();
        web.remember("The SphereScene contains a single Sphere MeshInstance3D node.", 0);
        web.remember("The CubeScene contains a Cube mesh and a directional light.", 0);
        web.remember("Greenhouse auto-plant now works correctly with full storage.", 0);
        web.remember("Venue finances now use real ingredient costs in the economy update.", 0);
        web.remember("User prefers concise answers and dark themes with soft contrast.", 0);
        web
    }

    #[test]
    fn tokenize_drops_stopwords_and_short_tokens() {
        let tokens = tokenize("The Sphere is in the scene and it has a node");
        assert!(tokens.contains(&"sphere".to_string()));
        assert!(tokens.contains(&"scene".to_string()));
        assert!(tokens.contains(&"node".to_string()));
        assert!(!tokens.contains(&"the".to_string()));
        assert!(!tokens.contains(&"is".to_string()));
    }

    #[test]
    fn similar_entries_cluster_spatially() {
        let mut web = DataWeb::default();
        let a = web.remember("Sphere mesh instance node in the sphere scene", 0);
        let b = web.remember("Sphere scene holds one sphere mesh node", 0);
        let c = web.remember("Completely unrelated cooking recipe about pasta sauce", 0);
        let dist = |x: u64, y: u64| {
            let p = web.nodes.iter().find(|n| n.id == x).unwrap();
            let q = web.nodes.iter().find(|n| n.id == y).unwrap();
            ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt()
        };
        // The two sphere memories sit closer together than the unrelated one.
        assert!(dist(a, b) < dist(a, c));
        // And they are linked as neighbors (a synapse), the unrelated one is not.
        let node_a = web.nodes.iter().find(|n| n.id == a).unwrap();
        assert!(node_a.neighbors.contains(&b));
        assert!(!node_a.neighbors.contains(&c));
    }

    #[test]
    fn recall_finds_direct_matches_first() {
        let mut web = seeded();
        let hits = web.recall("what node is in the sphere scene", 3, 0, true);
        assert!(!hits.is_empty());
        assert!(hits[0].content.contains("SphereScene"));
        // Reinforcement recorded a hit on the recalled node.
        assert!(web.nodes.iter().find(|n| n.content.contains("SphereScene")).unwrap().hits >= 1);
    }

    #[test]
    fn recall_surfaces_associated_neighbors_not_just_literal_matches() {
        let mut web = DataWeb::default();
        web.remember("Project Nivalis uses a greenhouse system for crops", 0);
        web.remember("The greenhouse auto-plant feature fills empty plots", 0);
        web.remember("Auto-plant respects full storage and refunds locked modules", 0);
        // Query shares terms with the first node; associated greenhouse nodes should also light up.
        let ids = web.recall_ids("nivalis project crops", 5, 0);
        assert!(ids.len() >= 2, "spreading activation should reach neighbors");
    }

    #[test]
    fn digest_is_bounded_regardless_of_store_size() {
        let mut web = DataWeb::default();
        for i in 0..500 {
            web.remember(&format!("Memory entry number {i} about scenes nodes meshes and systems"), 0);
        }
        assert_eq!(web.nodes.len(), 500);
        let digest = web.recall_digest("scenes nodes", 600, 0);
        assert!(digest.len() <= 600);
        assert!(!digest.is_empty());
    }

    #[test]
    fn duplicate_content_reinforces_instead_of_duplicating() {
        let mut web = DataWeb::default();
        let first = web.remember("The active scene is SphereScene", 0);
        let again = web.remember("The active scene is SphereScene", 10);
        assert_eq!(first, again);
        assert_eq!(web.nodes.len(), 1);
        assert_eq!(web.nodes[0].hits, 1);
    }

    #[test]
    fn pruning_keeps_the_store_bounded() {
        // Build a web just over capacity directly (fast), then one remember triggers pruning.
        let mut web = DataWeb::default();
        for i in 0..(MAX_NODES as u64 + 40) {
            web.nodes.push(MemoryNode {
                id: i + 1,
                content: format!("entry {i}"),
                keywords: vec![format!("token{i}")],
                x: 0.0,
                y: 0.0,
                neighbors: Vec::new(),
                hits: 0,
                created_at: 0,
                updated_at: 0,
            });
        }
        web.next_id = MAX_NODES as u64 + 41;
        assert!(web.nodes.len() > MAX_NODES);
        web.remember("a brand new distinctive memory about scenes", 100);
        assert!(web.nodes.len() <= MAX_NODES);
    }

    #[test]
    fn empty_query_returns_most_valuable_memories() {
        let mut web = seeded();
        // Reinforce one node heavily.
        for _ in 0..5 {
            web.recall("greenhouse", 1, 100, true);
        }
        let ids = web.recall_ids("", 2, 100);
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn roundtrips_through_json_without_losing_structure() {
        let web = seeded();
        let serialized = serde_json::to_string(&web).unwrap();
        let restored: DataWeb = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.nodes.len(), web.nodes.len());
        assert_eq!(restored.next_id, web.next_id);
    }
}
