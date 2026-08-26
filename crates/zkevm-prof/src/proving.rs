//! The proving time dataset document, as published.
//!
//! A dataset states how long each block of one corpus took to prove and on what machine, so the
//! costs a profile records are read against times measured over the same blocks.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Proving time dataset document, one wall clock time in milliseconds per block proved.
#[derive(Deserialize, Serialize)]
pub struct Proving {
    pub proving_time_ms: BTreeMap<String, u64>,
    pub meta: ProvingMeta,
}

/// What produced a set of proving times, which is the machine they were proved on.
#[derive(Deserialize, Serialize)]
pub struct ProvingMeta {
    /// What the dataset is called wherever the page offers it.
    pub name: String,
    /// Machines the proving ran across, every one of them the hardware below.
    pub machines: u32,
    pub hardware: Hardware,
}

/// One machine of the set that proved a dataset.
#[derive(Deserialize, Serialize)]
pub struct Hardware {
    pub cpu: String,
    pub ram_bytes: u64,
    pub os: String,
    /// Empty on a machine that proves on its CPU alone.
    #[serde(default)]
    pub gpus: Vec<String>,
}
