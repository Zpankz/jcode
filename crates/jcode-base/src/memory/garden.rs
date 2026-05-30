use super::{MemoryEntry, MemoryManager};
use crate::memory_graph::{EdgeKind, MemoryGraph, MemoryGraphScore};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DEDUP_SIMILARITY_THRESHOLD: f32 = 0.95;
const RELATIONSHIP_SIMILARITY_THRESHOLD: f32 = 0.78;
const MAX_CANDIDATES_PER_SCOPE: usize = 40;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GardenCandidateKind {
    Deduplicate,
    Relate,
    Prune,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryGardenCandidate {
    pub kind: GardenCandidateKind,
    pub scope: String,
    pub primary_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_id: Option<String>,
    pub score: f32,
    pub centrality: f32,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MemoryGardenReport {
    pub scanned_memories: usize,
    pub project_scores: Vec<MemoryGraphScore>,
    pub global_scores: Vec<MemoryGraphScore>,
    pub candidates: Vec<MemoryGardenCandidate>,
}

impl MemoryManager {
    /// Build a non-destructive memory garden worklist.
    ///
    /// The report is intentionally dry-run only: it identifies high-confidence
    /// deduplication, relationship, and prune candidates without mutating either
    /// memory graph. Ambient mode can log this safely, and a future approval flow
    /// can turn individual candidates into explicit graph operations.
    pub fn garden_dry_run(&self) -> Result<MemoryGardenReport> {
        let project = self.load_project_graph()?;
        let global = self.load_global_graph()?;

        let project_scores = project.memory_graph_scores();
        let global_scores = global.memory_graph_scores();
        let mut candidates = Vec::new();

        candidates.extend(garden_candidates_for_graph(
            "project",
            &project,
            &project_scores,
        ));
        candidates.extend(garden_candidates_for_graph(
            "global",
            &global,
            &global_scores,
        ));

        candidates.sort_by(|a, b| {
            candidate_priority(a.kind)
                .cmp(&candidate_priority(b.kind))
                .then_with(|| b.centrality.total_cmp(&a.centrality))
                .then_with(|| b.score.total_cmp(&a.score))
                .then_with(|| a.primary_id.cmp(&b.primary_id))
                .then_with(|| a.secondary_id.cmp(&b.secondary_id))
        });

        Ok(MemoryGardenReport {
            scanned_memories: project.memories.len() + global.memories.len(),
            project_scores,
            global_scores,
            candidates,
        })
    }
}

fn garden_candidates_for_graph(
    scope: &str,
    graph: &MemoryGraph,
    scores: &[MemoryGraphScore],
) -> Vec<MemoryGardenCandidate> {
    let centrality: HashMap<&str, f32> = scores
        .iter()
        .map(|score| (score.id.as_str(), score.centrality))
        .collect();
    let mut out = Vec::new();

    for memory in graph.memories.values() {
        if should_prune(memory) {
            out.push(MemoryGardenCandidate {
                kind: GardenCandidateKind::Prune,
                scope: scope.to_string(),
                primary_id: memory.id.clone(),
                secondary_id: None,
                score: 1.0 - memory.effective_confidence(),
                centrality: centrality.get(memory.id.as_str()).copied().unwrap_or(0.0),
                reason: if memory.active {
                    "low effective confidence and weak reinforcement".to_string()
                } else {
                    "inactive or superseded memory".to_string()
                },
            });
        }
    }

    let mut memories: Vec<&MemoryEntry> = graph
        .active_memories()
        .filter(|memory| memory.embedding.is_some())
        .collect();
    memories.sort_by(|a, b| a.id.cmp(&b.id));

    for i in 0..memories.len() {
        for j in (i + 1)..memories.len() {
            let Some(a_embedding) = memories[i].embedding.as_deref() else {
                continue;
            };
            let Some(b_embedding) = memories[j].embedding.as_deref() else {
                continue;
            };
            if a_embedding.len() != b_embedding.len() {
                continue;
            }
            let similarity = crate::embedding::cosine_similarity(a_embedding, b_embedding);
            let pair_centrality = centrality
                .get(memories[i].id.as_str())
                .copied()
                .unwrap_or(0.0)
                .max(
                    centrality
                        .get(memories[j].id.as_str())
                        .copied()
                        .unwrap_or(0.0),
                );

            if similarity >= DEDUP_SIMILARITY_THRESHOLD {
                out.push(MemoryGardenCandidate {
                    kind: GardenCandidateKind::Deduplicate,
                    scope: scope.to_string(),
                    primary_id: memories[i].id.clone(),
                    secondary_id: Some(memories[j].id.clone()),
                    score: similarity,
                    centrality: pair_centrality,
                    reason: "embedding similarity exceeds dedup threshold".to_string(),
                });
            } else if similarity >= RELATIONSHIP_SIMILARITY_THRESHOLD
                && !has_relates_edge(graph, &memories[i].id, &memories[j].id)
            {
                out.push(MemoryGardenCandidate {
                    kind: GardenCandidateKind::Relate,
                    scope: scope.to_string(),
                    primary_id: memories[i].id.clone(),
                    secondary_id: Some(memories[j].id.clone()),
                    score: similarity,
                    centrality: pair_centrality,
                    reason: "similar active memories are not explicitly related".to_string(),
                });
            }
        }
    }

    out.sort_by(|a, b| {
        candidate_priority(a.kind)
            .cmp(&candidate_priority(b.kind))
            .then_with(|| b.centrality.total_cmp(&a.centrality))
            .then_with(|| b.score.total_cmp(&a.score))
            .then_with(|| a.primary_id.cmp(&b.primary_id))
            .then_with(|| a.secondary_id.cmp(&b.secondary_id))
    });
    out.truncate(MAX_CANDIDATES_PER_SCOPE);
    out
}

fn should_prune(memory: &MemoryEntry) -> bool {
    !memory.active || (memory.effective_confidence() < 0.05 && memory.strength <= 1)
}

fn has_relates_edge(graph: &MemoryGraph, a: &str, b: &str) -> bool {
    graph
        .get_edges(a)
        .iter()
        .any(|edge| edge.target == b && matches!(edge.kind, EdgeKind::RelatesTo { .. }))
        || graph
            .get_edges(b)
            .iter()
            .any(|edge| edge.target == a && matches!(edge.kind, EdgeKind::RelatesTo { .. }))
}

fn candidate_priority(kind: GardenCandidateKind) -> u8 {
    match kind {
        GardenCandidateKind::Deduplicate => 0,
        GardenCandidateKind::Prune => 1,
        GardenCandidateKind::Relate => 2,
    }
}
