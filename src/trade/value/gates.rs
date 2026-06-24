//! Under-sampled gate candidates detected during model build (Task 7).
//! Stub: struct definition only; populated in a later task.

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct GateCandidate {
    pub stat_id: String,
    pub label: Option<String>,
    pub count: usize,
}
