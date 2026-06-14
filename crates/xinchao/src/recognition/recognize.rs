//! Face-embedding comparison and the accept/reject decision.
//!
//! Recognition models map a face crop to a fixed-length [`Embedding`]. Two faces
//! are compared by cosine distance; an attempt is accepted when the closest
//! enrolled embedding is within the configured threshold. This module is pure
//! math with no model or I/O dependency, so it is exhaustively unit-tested.
//!
//! # Security posture
//!
//! Comparisons fail closed: a dimension mismatch or a degenerate (zero-norm)
//! embedding yields the maximum distance, never a spurious match.

// Constants

/// Maximum possible cosine distance (returned on any degenerate comparison).
const MAX_DISTANCE: f32 = 2.0;

// Data Structures

/// A fixed-length face embedding produced by a recognition model.
#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    values: Vec<f32>,
}

/// The nearest enrolled embedding to a probe and whether it was accepted.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    /// Whether the nearest distance is within the threshold.
    pub accepted: bool,
    /// Cosine distance to the nearest enrolled embedding.
    pub distance: f32,
    /// Index of the nearest enrolled embedding.
    pub index: usize,
}

// Functions

/// Decides whether a probe matches any enrolled embedding within `threshold`.
///
/// Returns `None` only when there are no enrolled embeddings. Otherwise reports
/// the nearest embedding and whether its distance is within the threshold.
pub fn decide(probe: &Embedding, enrolled: &[Embedding], threshold: f32) -> Option<Decision> {
    let mut best_index = 0;
    let mut best_distance = MAX_DISTANCE;
    for (index, candidate) in enrolled.iter().enumerate() {
        let distance = probe.cosine_distance(candidate);
        if distance < best_distance {
            best_distance = distance;
            best_index = index;
        }
    }
    if enrolled.is_empty() {
        return None;
    }
    Some(Decision {
        accepted: best_distance <= threshold,
        distance: best_distance,
        index: best_index,
    })
}

impl Embedding {
    /// Wraps raw model output as an embedding.
    pub fn new(values: Vec<f32>) -> Self {
        Embedding { values }
    }

    /// Number of dimensions in the embedding.
    pub fn dim(&self) -> usize {
        self.values.len()
    }

    /// The raw embedding values.
    pub fn as_slice(&self) -> &[f32] {
        &self.values
    }

    /// Cosine distance (`1 - cosine similarity`) to another embedding.
    ///
    /// Returns [`MAX_DISTANCE`] for mismatched dimensions or a zero-norm
    /// embedding, so a malformed input can never read as a close match.
    pub fn cosine_distance(&self, other: &Embedding) -> f32 {
        if self.values.len() != other.values.len() || self.values.is_empty() {
            return MAX_DISTANCE;
        }
        let mut dot = 0.0f32;
        let mut norm_self = 0.0f32;
        let mut norm_other = 0.0f32;
        for (a, b) in self.values.iter().zip(other.values.iter()) {
            dot += a * b;
            norm_self += a * a;
            norm_other += b * b;
        }
        if norm_self == 0.0 || norm_other == 0.0 {
            return MAX_DISTANCE;
        }
        let similarity = dot / (norm_self.sqrt() * norm_other.sqrt());
        (1.0 - similarity).clamp(0.0, MAX_DISTANCE)
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_embeddings_have_zero_distance() {
        let a = Embedding::new(vec![1.0, 2.0, 3.0]);
        let b = Embedding::new(vec![1.0, 2.0, 3.0]);
        assert!(a.cosine_distance(&b) < 1e-6);
    }

    #[test]
    fn scaled_embeddings_have_zero_distance() {
        // Cosine distance ignores magnitude.
        let a = Embedding::new(vec![1.0, 0.0]);
        let b = Embedding::new(vec![5.0, 0.0]);
        assert!(a.cosine_distance(&b) < 1e-6);
    }

    #[test]
    fn orthogonal_embeddings_have_unit_distance() {
        let a = Embedding::new(vec![1.0, 0.0]);
        let b = Embedding::new(vec![0.0, 1.0]);
        assert!((a.cosine_distance(&b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn opposite_embeddings_have_max_distance() {
        let a = Embedding::new(vec![1.0, 0.0]);
        let b = Embedding::new(vec![-1.0, 0.0]);
        assert!((a.cosine_distance(&b) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn dimension_mismatch_is_max_distance() {
        let a = Embedding::new(vec![1.0, 2.0]);
        let b = Embedding::new(vec![1.0, 2.0, 3.0]);
        assert_eq!(a.cosine_distance(&b), MAX_DISTANCE);
    }

    #[test]
    fn zero_norm_is_max_distance() {
        let a = Embedding::new(vec![0.0, 0.0]);
        let b = Embedding::new(vec![1.0, 1.0]);
        assert_eq!(a.cosine_distance(&b), MAX_DISTANCE);
    }

    #[test]
    fn decide_returns_none_without_enrollments() {
        let probe = Embedding::new(vec![1.0, 0.0]);
        assert_eq!(decide(&probe, &[], 0.45), None);
    }

    #[test]
    fn decide_accepts_close_match_and_picks_nearest() {
        let probe = Embedding::new(vec![1.0, 0.0]);
        let enrolled = vec![
            Embedding::new(vec![0.0, 1.0]),  // distance 1.0
            Embedding::new(vec![1.0, 0.05]), // distance ~0.0
        ];
        let decision = decide(&probe, &enrolled, 0.45).unwrap();
        assert_eq!(decision.index, 1);
        assert!(decision.accepted);
    }

    #[test]
    fn decide_rejects_when_beyond_threshold() {
        let probe = Embedding::new(vec![1.0, 0.0]);
        let enrolled = vec![Embedding::new(vec![0.0, 1.0])]; // distance 1.0
        let decision = decide(&probe, &enrolled, 0.45).unwrap();
        assert!(!decision.accepted);
    }
}
