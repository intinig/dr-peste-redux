//! Similarity-weight parameters for the k-NN estimate (Task 6).
//! Stub: fields populated to zero by Default; training fills them.

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Default)]
pub struct SimWeights {
    pub jaccard: f64,
    pub roll: f64,
}
